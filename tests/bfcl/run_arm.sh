#!/usr/bin/env bash
# Run one BFCL AST arm end-to-end on LFM2-Q2 through openharn, clean and repeatable.
# Assumes: llama-server already up on :8080; venv active; id file in $IDSRC.
# Usage: run_arm.sh <arm_name> <serve_port> "<extra openharn env>"
#   e.g. run_arm.sh D    8090 "OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_STRICT_ABSTAIN=1 OPENHARN_FC_GATE=1"
#        run_arm.sh H1   8091 "OPENHARN_NATIVE_TEMPLATE=1"
#        run_arm.sh H2   8092 "OPENHARN_NATIVE_TEMPLATE=1 OPENHARN_PLAN_FIRST=1"
set -u
ARM="$1"; PORT="$2"; EXTRA="$3"
ROOT="$4"                     # BFCL_PROJECT_ROOT for this arm
IDSRC="$5"                    # path to test_case_ids_to_generate.json
BIN="./target/debug/openharn.exe"

mkdir -p "$ROOT"
cp "$IDSRC" "$ROOT/test_case_ids_to_generate.json"

# start openharn serve for this arm
env OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_SERVE=1 OPENHARN_SERVE_PORT=$PORT \
    OPENHARN_FC_PROXY=1 OPENHARN_MAX_TOKENS=512 $EXTRA \
    "$BIN" . > "$TEMP/openharn_${ARM}.log" 2>&1 &
SERVE_PID=$!

# wait for serve
for i in $(seq 1 30); do
  s=$(curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:$PORT/v1/models 2>/dev/null)
  [ "$s" = "200" ] && break
  sleep 2
done

export BFCL_PROJECT_ROOT="$ROOT" PYTHONUTF8=1 PYTHONIOENCODING=utf-8 OPENAI_API_KEY=dummy \
       OPENAI_BASE_URL=http://127.0.0.1:$PORT/v1
echo "=== ARM $ARM : generate ==="
bfcl generate --model openharn-lfm2-harness --run-ids --num-threads 4 --temperature 0.001 -o > "$TEMP/gen_${ARM}.log" 2>&1
echo "=== ARM $ARM : evaluate ==="
bfcl evaluate --model openharn-lfm2-harness --partial-eval 2>&1 | grep -iE "accuracy|simple|multiple|parallel|irrelevance|Total|Count" | head -40

kill $SERVE_PID 2>/dev/null
echo "=== ARM $ARM : transport failures = $(grep -c 'giving up after 3' "$TEMP/openharn_${ARM}.log" 2>/dev/null) (retries used: $(grep -c 'attempt' "$TEMP/openharn_${ARM}.log" 2>/dev/null)) ==="
