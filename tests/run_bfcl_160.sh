#!/bin/bash
# Run the BFCL 160-entry AST benchmark
set -m  # enable job control

LLAMA_PORT=8081
FC_PORT=8090
MODEL="/home/paper/Downloads/LFM2-8B-A1B-UD-Q2_K_XL.gguf"
LLAMA_BIN="/home/paper/.local/share/com.paper.myelin/bin/cpu/llama-server"
OPENHARN_BIN="/home/paper/openharn/target/debug/openharn"
BENCH_SCRIPT="/home/paper/openharn/tests/bench_bfcl_160.py"

cleanup() {
    kill $LLAMA_PID $FC_PID 2>/dev/null
    wait $LLAMA_PID $FC_PID 2>/dev/null
}
trap cleanup EXIT

# Start llama-server
echo "Starting llama-server on :$LLAMA_PORT ..."
$LLAMA_BIN -m "$MODEL" --jinja --ctx-size 16384 -ngl 0 --host 127.0.0.1 --port $LLAMA_PORT --no-warmup > /tmp/llama_bfcl.log 2>&1 &
LLAMA_PID=$!
for i in $(seq 1 30); do
    if curl -s http://127.0.0.1:$LLAMA_PORT/health 2>/dev/null | grep -q ok; then
        echo "  llama-server ready (${i}s)"
        break
    fi
    sleep 2
done

# Start openharn FC-proxy
echo "Starting openharn FC-proxy on :$FC_PORT ..."
OPENHARN_BASE_URL=http://127.0.0.1:$LLAMA_PORT/v1 \
OPENHARN_SERVE=1 OPENHARN_SERVE_PORT=$FC_PORT \
OPENHARN_FC_PROXY=1 OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 \
OPENHARN_STRICT_ABSTAIN=1 OPENHARN_FC_GATE=1 OPENHARN_MAX_TOKENS=512 \
"$OPENHARN_BIN" . > /tmp/fc_bfcl.log 2>&1 &
FC_PID=$!
for i in $(seq 1 10); do
    if curl -s http://127.0.0.1:$FC_PORT/health 2>/dev/null | grep -q ok; then
        echo "  FC-proxy ready (${i}s)"
        break
    fi
    sleep 1
done

echo ""
echo "=== Running BFCL 160-entry AST benchmark ==="
echo ""

python3 "$BENCH_SCRIPT" 2>&1
exit $?
