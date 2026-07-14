#!/usr/env bash
# tune_model.sh — find the best openharn config (env vars + server flags ONLY,
# never the system prompt) for a given GGUF on this machine.
#
# It launches llama-server, probes whether the model does native tool-calling and
# whether it thinks by default, then runs tests/behavior.py across the candidate
# configs and picks the one with the highest pass score (tie-break: speed).
#
# The candidate set is generated exhaustively from the harness flag matrix
# (prompt_tools, strict, narrow, yesno, friendly) subject to the same
# implication rules as agent.rs, crossed with thinking on/off — so it generalizes
# to any model, not just the ones hand-tested.
#
# Accuracy / runtime tradeoff (pick one):
#   --full     every valid flag combo x thinking on/off, full 6-case suite (exhaustive, slow)
#   --pruned   probe-decided subset, full 6-case suite                (default, balanced)
#   --quick    representative subset, 4 representative cases              (fastest)
#
# Usage:
#   tests/tune_model.sh <model.gguf> [--full|--pruned|--quick] [--port N] [--llama PATH] [--ctx N]
#
# Output: prints the winning config + a ranking table, and saves a log to
# tests/tune_logs/<model>.md.
#
# llama-server: REQUIRED (preinstalled). Located via --llama, $LLAMA_SERVER,
# PATH, or the known myelin cpu build; errors clearly if none found.

set -u
MODE="${1:-}"
MODE="${MODE/#\~/$HOME}"
[ -f "$MODE" ] || { echo "usage: $0 <model.gguf> [--full|--pruned|--quick]"; exit 2; }
shift || true

LLAMA_ARG=""
PORT=8080
CTX=16384
MODE_FLAG="pruned"
while [ $# -gt 0 ]; do
  case "$1" in
    --full)   MODE_FLAG="full" ;;
    --pruned) MODE_FLAG="pruned" ;;
    --quick)  MODE_FLAG="quick" ;;
    --port)  PORT="${2:-8080}"; shift ;;
    --llama) LLAMA_ARG="$2"; shift ;;
    --ctx)   CTX="$2"; shift ;;
    *) echo "unknown arg: $1"; exit 2 ;;
  esac
  shift || true
done

# ---- llama-server detection (must be preinstalled) --------------------------
detect_llama () {
  if [ -n "${LLAMA_ARG:-}" ]; then LLAMA="$LLAMA_ARG"
  elif [ -n "${LLAMA_SERVER:-}" ]; then LLAMA="$LLAMA_SERVER"
  elif command -v llama-server >/dev/null 2>&1; then LLAMA="llama-server"
  elif [ -x /home/paper/.local/share/com.paper.myelin/bin/cpu/llama-server ]; then
       LLAMA="/home/paper/.local/share/com.paper.myelin/bin/cpu/llama-server"
  else
    echo "ERROR: llama-server binary not found." >&2
    echo "  Install llama.cpp, pass --llama <path>, or set LLAMA_SERVER." >&2
    exit 1
  fi
}
detect_llama
echo "using llama-server: $LLAMA"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXE="$ROOT/target/debug/openharn"
[ -x "$EXE" ] || { echo "build first: cargo build (missing $EXE)"; exit 2; }
mkdir -p "$ROOT/tests/tune_logs"
STEM="$(basename "$MODE" | sed 's/\.gguf$//')"
LOG="$ROOT/tests/tune_logs/$STEM.md"

# ---- server control ----------------------------------------------------------
# Clear any stale server bound to our port (otherwise probes run against the
# wrong model and every result is bogus). Kill BY PORT so we never match this
# script's own command line (which references the llama-server binary path).
if command -v fuser >/dev/null 2>&1; then fuser -k "${PORT}/tcp" >/dev/null 2>&1; fi
sleep 1
SRV_PID=""
SRV_THINK=""
start_server () {        # $1 = on|off
  stop_server
  local kw=""
  [ "$1" = "off" ] && kw='--chat-template-kwargs {"enable_thinking":false}'
  nohup "$LLAMA" -m "$MODE" --jinja --ctx-size "$CTX" -ngl 0 \
    --host 127.0.0.1 --port "$PORT" --no-warmup $kw >/tmp/openharn-tune.log 2>&1 &
  SRV_PID=$!
  for _ in $(seq 1 90); do
    curl -s -m2 "http://127.0.0.1:$PORT/health" 2>/dev/null | grep -q '"status":"ok"' && break
    sleep 1
  done
  SRV_THINK="$1"
}
stop_server () { [ -n "${SRV_PID:-}" ] && kill "$SRV_PID" 2>/dev/null; SRV_PID=""; sleep 1; }

# ---- probes ----------------------------------------------------------------
probe_thinks () {      # 0=no thinking, 1=thinks by default
  curl -s -m90 "http://127.0.0.1:$PORT/v1/chat/completions" -H 'Content-Type: application/json' \
    -d '{"model":"local","stream":false,"temperature":0.2,"messages":[{"role":"user","content":"Reply with exactly: OK"}]}' \
  | python3 -c 'import sys,json
try:
    m=json.load(sys.stdin)["choices"][0]["message"]
    print(1 if (m.get("reasoning_content") or "").strip() else 0)
except Exception:
    print(0)'
}
probe_native () {      # 0=native tools fail, 1=native tool_calls work
  curl -s -m90 "http://127.0.0.1:$PORT/v1/chat/completions" -H 'Content-Type: application/json' -d '{
    "model":"local","stream":false,"temperature":0.2,
    "messages":[{"role":"system","content":"You are a coding agent."},
                {"role":"user","content":"Search for the word Config and say which file defines it."}],
    "tools":[{"type":"function","function":{"name":"grep","description":"regex search","parameters":{"type":"object","properties":{"pattern":{"type":"string"}},"required":["pattern"]}}}],
    "tool_choice":"auto"}' \
  | python3 -c 'import sys,json
try:
    tc=json.load(sys.stdin)["choices"][0]["message"].get("tool_calls") or []
    print(1 if tc else 0)
except Exception:
    print(0)'
}

# ---- run one candidate ------------------------------------------------------
# $1 = env string, $2 = case filter (or "" for all)
run_case () {
  env -u OPENHARN_STRICT_TOOLS -u OPENHARN_PROMPT_TOOLS -u OPENHARN_NARROW \
       -u OPENHARN_YESNO -u OPENHARN_FRIENDLY_RESULTS -u OPENHARN_NO_THINK \
       OPENHARN_BASE_URL="http://127.0.0.1:$PORT/v1" OPENHARN_MODEL="$STEM" \
       $1 ${2:+OPENHARN_TUNE_CASES="$2"} \
       timeout 580 python3 "$ROOT/tests/behavior.py" "$PORT" 2>&1
}

# ---- exhaustive candidate generator ----------------------------------------
# Emits "name|env|thinking" for every valid flag combo x thinking.
# Implication rules mirror agent.rs: strict=>prompt_tools, narrow=>strict,
# friendly=>prompt_tools; narrow conflicts with yesno/friendly.
gen_all () {
  for pt in 0 1; do for st in 0 1; do for na in 0 1; do for ye in 0 1; do for fr in 0 1; do
    [ "$st" = 1 ] && [ "$pt" = 0 ] && continue
    [ "$na" = 1 ] && [ "$st" = 0 ] && continue
    [ "$fr" = 1 ] && [ "$pt" = 0 ] && continue
    [ "$na" = 1 ] && [ "$ye" = 1 ] && continue
    [ "$na" = 1 ] && [ "$fr" = 1 ] && continue
    env=""
    [ "$pt" = 1 ] && env="$env OPENHARN_PROMPT_TOOLS=1"
    [ "$st" = 1 ] && env="$env OPENHARN_STRICT_TOOLS=1"
    [ "$na" = 1 ] && env="$env OPENHARN_NARROW=1"
    [ "$ye" = 1 ] && env="$env OPENHARN_YESNO=1"
    [ "$fr" = 1 ] && env="$env OPENHARN_FRIENDLY_RESULTS=1"
    name="pt$pt st$st na$na ye$ye fr$fr"
    echo "$name|$env|on"
    echo "$name|$env|off"
  done; done; done; done; done
}

QUICK_CASES="greeting_uses_no_tools,find_file_uses_glob_not_grep,missing_file_is_reported_not_faked,edits_real_file_via_anchor"

# ---- probe + build candidate list ------------------------------------------
NATIVE_T="$(start_server on; probe_thinks)"
NATIVE="$(probe_native)"
echo "== probe: thinks_by_default=$NATIVE_T  native_tool_calls=$NATIVE =="

CAND="$(gen_all)"
# a non-native model can't use the bare native-default config (no tool calls)
[ "$NATIVE" = "0" ] && CAND="$(printf '%s\n' "$CAND" | grep -v '^pt0 st0 na0 ye0 fr0|')"

case "$MODE_FLAG" in
  full)   : ;;   # all valid combos, full suite
  pruned)
    if [ "$NATIVE" = "1" ]; then
      CAND="$(printf '%s\n' "$CAND" | grep -E '^pt0 st0 na0 ye0 fr0\||^pt0 st0 na0 ye0 fr1\|')";
    else
      CAND="$(printf '%s\n' "$CAND" | grep -E '^pt1 st1 na0 ye0 fr0\||^pt1 st1 na0 ye1 fr0\|')";
    fi ;;
  quick)
    if [ "$NATIVE" = "1" ]; then
      CAND="$(printf '%s\n' "$CAND" | grep -E '^pt0 st0 na0 ye0 fr0\|')";
    else
      CAND="$(printf '%s\n' "$CAND" | grep -E '^pt1 st1 na0 ye0 fr0\||^pt1 st1 na0 ye1 fr0\||^pt1 st0 na0 ye0 fr0\|')";
    fi ;;
esac
[ -z "$(printf '%s' "$CAND" | grep -v '^$')" ] && { echo "no candidates selected for mode=$MODE_FLAG"; stop_server; exit 1; }
CASE_FILT=""
[ "$MODE_FLAG" = "quick" ] && CASE_FILT="$QUICK_CASES"

# group by thinking (all "on" first) so the server restarts at most once
CAND="$(printf '%s\n' "$CAND" | grep -v '^$' | sort -t'|' -k3)"

# ---- run ----------------------------------------------------------------
declare -a RES_NAME=() RES_ENV=() RES_THINK=() RES_SCORE=() RES_TIME=() RES_TOTAL=()
echo; echo "== running $(printf '%s\n' "$CAND" | grep -c .) candidates ($MODE_FLAG) =="
while IFS='|' read -r name env think; do
  [ -z "$name" ] && continue
  [ "$think" = "$SRV_THINK" ] || start_server "$think"
  t0=$(date +%s)
  out="$(run_case "$env" "$CASE_FILT")"
  dt=$(( $(date +%s) - t0 ))
  score=$(echo "$out" | grep -oE '[0-9]+/[0-9]+ passed' | tail -1 | cut -d/ -f1)
  total=$(echo "$out" | grep -oE '[0-9]+/[0-9]+ passed' | tail -1 | cut -d/ -f2)
  [ -z "${score:-}" ] && score=0
  [ -z "${total:-}" ] && total=6
  echo "  $name (think=$think) -> $score/$total  (${dt}s)"
  RES_NAME+=("$name"); RES_ENV+=("$env"); RES_THINK+=("$think")
  RES_SCORE+=("$score"); RES_TIME+=("$dt"); RES_TOTAL+=("$total")
done < <(printf '%s\n' "$CAND")

# ---- rank: max score, then min time --------------------------------------
BEST=0
for i in "${!RES_NAME[@]}"; do
  if [ "${RES_SCORE[$i]}" -gt "${RES_SCORE[$BEST]}" ] \
     || { [ "${RES_SCORE[$i]}" = "${RES_SCORE[$BEST]}" ] && [ "${RES_TIME[$i]}" -lt "${RES_TIME[$BEST]}" ]; }; then
    BEST=$i
  fi
done

# ---- reconstruct recommended block from the winner -------------------------
win_name="${RES_NAME[$BEST]}"; win_env="${RES_ENV[$BEST]}"; win_think="${RES_THINK[$BEST]}"
think_flag=""
[ "$win_think" = "off" ] && think_flag='--chat-template-kwargs '"'"'{"enable_thinking":false}'"'"''

# ---- write per-model config file (KEY=value list openharn can load) ------
# This is the file the user passes to openharn (--config) so they don't have
# to retype the tuned OPENHARN_* flags, and it doubles as the record of what
# worked best for this model.
mkdir -p "$ROOT/configs"
CONF="$ROOT/configs/$STEM.conf"
{
  echo "# openharn config for $STEM"
  echo "# generated by tests/tune_model.sh — best-scoring config (score ${RES_SCORE[$BEST]}/${RES_TOTAL[$BEST]}, ${RES_TIME[$BEST]}s)"
  echo "# server: llama-server -m $(basename "$MODE") --jinja --ctx-size $CTX -ngl 0 --host 127.0.0.1 --port $PORT --no-warmup $think_flag"
  echo "# run:    OPENHARN_MODEL=$STEM ./target/debug/openharn . --config $CONF"
  for kv in $win_env; do
    echo "$kv"
  done
} > "$CONF"

# ---- print + save ---------------------------------------------------------
{
echo "# tune: $STEM"
echo
echo "probe: thinks_by_default=$NATIVE_T  native_tool_calls=$NATIVE   mode=$MODE_FLAG"
echo
echo "## winner: $win_name  (${RES_SCORE[$BEST]}/${RES_TOTAL[$BEST]}, ${RES_TIME[$BEST]}s)"
echo
echo '```sh'
echo "# server"
echo "llama-server -m $(basename "$MODE") --jinja --ctx-size $CTX -ngl 0 --host 127.0.0.1 --port $PORT --no-warmup $think_flag"
echo "# openharn"
[ -n "$win_env" ] && echo "export$win_env"
echo "$ROOT/target/debug/openharn ."
echo '```'
echo
echo "## ranking"
echo
echo "| config | think | score | time s |"
echo "|---|---|---|---|"
for i in "${!RES_NAME[@]}"; do
  echo "| ${RES_NAME[$i]} | ${RES_THINK[$i]} | ${RES_SCORE[$i]}/${RES_TOTAL[$i]} | ${RES_TIME[$i]} |"
done
} | tee "$LOG"

stop_server
echo; echo "log saved: $LOG"
echo "config saved: $CONF"
echo "run: OPENHARN_MODEL=$STEM ./target/debug/openharn . --config $CONF"
