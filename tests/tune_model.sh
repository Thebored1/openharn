#!/usr/env bash
# tune_model.sh — find the best openharn config (env vars + server flags ONLY,
# never the system prompt) for a given GGUF on this machine.
#
# It launches llama-server, probes whether the model does native tool-calling and
# whether it thinks by default, then runs tests/behavior.py across the candidate
# configs and picks the one with the highest pass score (tie-break: speed).
#
# Accuracy / runtime tradeoff (pick one):
#   --pruned   probe-decided 2-4 configs, full 6-case suite  (default, balanced)
#   --full     every candidate config, full 6-case suite      (slowest, thorough)
#   --quick     all candidates, 3 representative cases            (fastest, broad)
#
# Usage:
#   tests/tune_model.sh <model.gguf> [--pruned|--full|--quick] [--port N] [--llama PATH] [--ctx N]
#
# Output: prints the winning config + a ranking table, and saves a log to
# tests/tune_logs/<model>.md.

set -u
MODE="${1:-}"
MODE="${MODE/#\~/$HOME}"
[ -f "$MODE" ] || { echo "usage: $0 <model.gguf> [--pruned|--full|--quick]"; exit 2; }
shift || true

MODE_ARG=""
PORT=8080
LLAMA="${LLAMA_SERVER:-llama-server}"
CTX=16384
MODE_FLAG="pruned"
while [ $# -gt 0 ]; do
  case "$1" in
    --pruned) MODE_FLAG="pruned" ;;
    --full)   MODE_FLAG="full" ;;
    --quick)  MODE_FLAG="quick" ;;
    --port)  PORT="${2:-8080}"; shift ;;
    --llama) LLAMA="$2"; shift ;;
    --ctx)   CTX="$2"; shift ;;
    *) echo "unknown arg: $1"; exit 2 ;;
  esac
  shift || true
done

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
if command -v fuser >/dev/null 2>&1; then fuser -k "${PORT}/tcp" 2>/dev/null; fi
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
# $1 = env string (NAME=val space-separated), $2 = case filter (or "" for all)
run_case () {
  local env="$1" filt="$2"
  env -u OPENHARN_STRICT_TOOLS -u OPENHARN_PROMPT_TOOLS -u OPENHARN_NARROW \
       -u OPENHARN_YESNO -u OPENHARN_FRIENDLY_RESULTS -u OPENHARN_NO_THINK \
       OPENHARN_BASE_URL="http://127.0.0.1:$PORT/v1" OPENHARN_MODEL="$STEM" \
       $env ${filt:+OPENHARN_TUNE_CASES="$filt"} \
       timeout 580 python3 "$ROOT/tests/behavior.py" "$PORT" 2>&1
}

# ---- candidate list ---------------------------------------------------------
# each: "name|env|thinking"  (thinking: on/off/keep)
NATIVE_T="$(start_server on; probe_thinks)"
NATIVE="$(probe_native)"
echo "== probe: thinks_by_default=$NATIVE_T  native_tool_calls=$NATIVE =="

CAND=""
if [ "$NATIVE" = "1" ]; then
  CAND+="native_think_on||on"$'\n'
  CAND+="native_think_off||off"$'\n'
  CAND+="native_think_on_friendly|OPENHARN_FRIENDLY_RESULTS=1 OPENHARN_PROMPT_TOOLS=1|on"$'\n'
else
  # note: NARROW is a subset of PROMPT_TOOLS+STRICT (read/grep/glob only), so it
  # is never strictly better — excluded from auto-testing. Use it manually via
  # adapting-openharn.md when a read-only agent is wanted.
  CAND+="prompt_tools|OPENHARN_PROMPT_TOOLS=1|keep"$'\n'
  CAND+="prompt_tools_strict|OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1|keep"$'\n'
  CAND+="yesno_strict|OPENHARN_YESNO=1 OPENHARN_STRICT_TOOLS=1|keep"$'\n'
fi

# ---- mode: pick configs + cases --------------------------------------------
CASE_FILT=""
if [ "$MODE_FLAG" = "quick" ]; then
  # include an edit case so a read-only config (narrow) is penalized vs a
  # full one (prompt_tools_strict) that can also write/edit.
  CASE_FILT="greeting_uses_no_tools,find_file_uses_glob_not_grep,missing_file_is_reported_not_faked,edits_real_file_via_anchor"
elif [ "$MODE_FLAG" = "pruned" ]; then
  if [ "$NATIVE" = "1" ]; then
    CAND="$(printf '%s' "$CAND" | grep -E '^native_think_on\||^native_think_off\|')"
  else
    CAND="$(printf '%s' "$CAND" | grep -E '^prompt_tools_strict\||^yesno_strict\|')"
  fi
fi
# --full keeps the whole CAND list and all 6 cases.

# ---- run ----------------------------------------------------------------
declare -a RES_NAME=() RES_SCORE=() RES_TIME=() RES_TOTAL=()
echo; echo "== running $(printf '%s' "$CAND" | grep -c .) candidates ($MODE_FLAG) =="
while IFS='|' read -r name env think; do
  [ -z "$name" ] && continue
  [ "$think" = "keep" ] || { [ "$think" != "$SRV_THINK" ] && start_server "$think"; }
  t0=$(date +%s)
  out="$(run_case "$env" "$CASE_FILT")"
  dt=$(( $(date +%s) - t0 ))
  score=$(echo "$out" | grep -oE '[0-9]+/[0-9]+ passed' | tail -1 | cut -d/ -f1)
  total=$(echo "$out" | grep -oE '[0-9]+/[0-9]+ passed' | tail -1 | cut -d/ -f2)
  [ -z "${score:-}" ] && score=0
  [ -z "${total:-}" ] && total=6
  echo "  $name -> $score/$total  (${dt}s)"
  RES_NAME+=("$name"); RES_SCORE+=("$score"); RES_TIME+=("$dt"); RES_TOTAL+=("$total")
done < <(printf '%s' "$CAND")

# ---- rank: max score, then min time --------------------------------------
BEST=0; BEST_I=0
for i in "${!RES_NAME[@]}"; do
  if [ "${RES_SCORE[$i]}" -gt "${RES_SCORE[$BEST_I]}" ] \
     || { [ "${RES_SCORE[$i]}" = "${RES_SCORE[$BEST_I]}" ] && [ "${RES_TIME[$i]}" -lt "${RES_TIME[$BEST_I]}" ]; }; then
    BEST_I=$i
  fi
done

# ---- map winner name -> recommended env + server flag ------------------------
win_name="${RES_NAME[$BEST_I]}"
rec_env=""; rec_think="on"
case "$win_name" in
  native_think_on)            rec_env=""; rec_think="on" ;;
  native_think_off)           rec_env=""; rec_think="off" ;;
  native_think_on_friendly)    rec_env="OPENHARN_FRIENDLY_RESULTS=1 OPENHARN_PROMPT_TOOLS=1"; rec_think="on" ;;
  prompt_tools)               rec_env="OPENHARN_PROMPT_TOOLS=1"; rec_think="keep" ;;
  prompt_tools_strict)        rec_env="OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1"; rec_think="keep" ;;
  yesno_strict)              rec_env="OPENHARN_YESNO=1 OPENHARN_STRICT_TOOLS=1"; rec_think="keep" ;;
  narrow)                    rec_env="OPENHARN_NARROW=1"; rec_think="keep" ;;
esac
think_flag=""
[ "$rec_think" = "off" ] && think_flag='--chat-template-kwargs '"'"'{"enable_thinking":false}'"'"''

# ---- print + save ---------------------------------------------------------
{
echo "# tune: $STEM"
echo
echo "probe: thinks_by_default=$NATIVE_T  native_tool_calls=$NATIVE   mode=$MODE_FLAG"
echo
echo "## winner: $win_name  (${RES_SCORE[$BEST_I]}/${RES_TOTAL[$BEST_I]}, ${RES_TIME[$BEST_I]}s)"
echo
echo '```sh'
echo "# server"
echo "llama-server -m $(basename "$MODE") --jinja --ctx-size $CTX -ngl 0 --host 127.0.0.1 --port $PORT --no-warmup $think_flag"
echo "# openharn"
[ -n "$rec_env" ] && echo "export $rec_env"
echo "$ROOT/target/debug/openharn ."
echo '```'
echo
echo "## ranking"
echo
echo "| config | score | time s |"
echo "|---|---|---|"
for i in "${!RES_NAME[@]}"; do
  echo "| ${RES_NAME[$i]} | ${RES_SCORE[$i]}/${RES_TOTAL[$i]} | ${RES_TIME[$i]} |"
done
} | tee "$LOG"

stop_server
echo; echo "log saved: $LOG"
