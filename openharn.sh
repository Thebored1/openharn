#!/usr/bin/env bash
# openharn launcher for Linux / macOS. Starts a local llama-server (if one isn't
# already answering) and opens the REPL.
#
#   ./openharn.sh [dir]            fast mode (default)
#   ./openharn.sh [dir] --think    enable the model's thinking mode
#
# Override paths via env:
#   OPENHARN_GGUF   path to the .gguf model   (default: ~/Downloads/MiniCPM-V-4_6-Q8_0.gguf)
#   LLAMA_SERVER    llama-server binary        (default: llama-server on PATH)
#   OPENHARN_PORT   port                       (default: 8080)

DIR="."
THINK=0
for a in "$@"; do
  case "$a" in
    --think) THINK=1 ;;
    *)       DIR="$a" ;;
  esac
done

PORT="${OPENHARN_PORT:-8080}"
MODEL="${OPENHARN_GGUF:-$HOME/Downloads/MiniCPM-V-4_6-Q8_0.gguf}"
LLAMA_SERVER="${LLAMA_SERVER:-llama-server}"
HERE="$(cd "$(dirname "$0")" && pwd)"
EXE="$HERE/target/debug/openharn"

[ -x "$EXE" ] || ( cd "$HERE" && cargo build )

if curl -s -m2 "http://127.0.0.1:$PORT/health" 2>/dev/null | grep -q '"status":"ok"'; then
  echo "reusing model already on :$PORT"
else
  echo "starting model on :$PORT ..."
  args=( -m "$MODEL" --jinja --ctx-size 16384 -ngl 99 --host 127.0.0.1 --port "$PORT" --no-warmup )
  [ "$THINK" = 1 ] && args+=( --chat-template-kwargs '{"enable_thinking":true}' --reasoning-format deepseek )
  "$LLAMA_SERVER" "${args[@]}" >/tmp/openharn-llama.log 2>&1 &
  for _ in $(seq 1 60); do
    curl -s -m2 "http://127.0.0.1:$PORT/health" 2>/dev/null | grep -q '"status":"ok"' && break
    sleep 1
  done
  echo "model ready."
fi

export OPENHARN_BASE_URL="http://127.0.0.1:$PORT/v1"
export OPENHARN_MODEL="${OPENHARN_MODEL:-minicpm}"
exec "$EXE" "$DIR"
