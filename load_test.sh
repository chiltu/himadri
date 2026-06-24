#!/usr/bin/env bash
# Load Test Script for himadri
# Usage: ./load_test.sh [concurrency] [total_requests] [gateway_port] [sink_port]

set -euo pipefail

CONCURRENCY=${1:-50}
TOTAL=${2:-100}
GATEWAY_PORT=${3:-8080}
SINK_PORT=${4:-8081}
GATEWAY_URL="http://localhost:${GATEWAY_PORT}"
SINK_URL="http://localhost:${SINK_PORT}"

echo "╔══════════════════════════════════════════╗"
echo "║  himadri Load Test                  ║"
echo "╠══════════════════════════════════════════╣"
echo "║  Gateway:    ${GATEWAY_URL}"
echo "║  Sink:       ${SINK_URL}"
echo "║  Concurrency: ${CONCURRENCY}"
echo "║  Total:      ${TOTAL} requests"
echo "╚══════════════════════════════════════════╝"
echo ""

# Check if gateway is running
if ! curl -sf "${GATEWAY_URL}/health" > /dev/null 2>&1; then
    echo "ERROR: Gateway not running at ${GATEWAY_URL}"
    echo "Start with: cargo run -p himadri"
    exit 1
fi

echo "Gateway is healthy. Starting load test..."
echo ""

# Run load test using xargs for concurrency
START_TIME=$(date +%s%N)

seq 1 "$TOTAL" | xargs -P "$CONCURRENCY" -I {} bash -c '
    RESPONSE=$(curl -s -w "\n%{http_code}\n%{time_total}" \
        -X POST "${0}/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer test-key" \
        -d "{\"model\":\"mock-openai\",\"messages\":[{\"role\":\"user\",\"content\":\"Hello\"}],\"stream\":false}" \
        2>/dev/null)
    
    HTTP_CODE=$(echo "$RESPONSE" | tail -2 | head -1)
    TIME_TOTAL=$(echo "$RESPONSE" | tail -1)
    
    echo "${HTTP_CODE} ${TIME_TOTAL}"
' "${GATEWAY_URL}" > /tmp/himadri_results.txt

END_TIME=$(date +%s%N)
ELAPSED_MS=$(( (END_TIME - START_TIME) / 1000000 ))

# Parse results
SUCCESS=$(grep -c "^200" /tmp/himadri_results.txt || true)
FAILED=$(grep -c -v "^200" /tmp/himadri_results.txt || true)
AVG_TIME=$(awk '{sum += $2; count++} END {printf "%.3f", sum/count}' /tmp/himadri_results.txt 2>/dev/null || echo "0")
P95_TIME=$(sort -t' ' -k2 -n /tmp/himadri_results.txt | awk -v p=0.95 'NR==int(NR*p){print $2}' 2>/dev/null || echo "0")
RPS=$(echo "scale=2; $TOTAL * 1000 / $ELAPSED_MS" | bc 2>/dev/null || echo "0")

echo ""
echo "╔══════════════════════════════════════════╗"
echo "║  Results                                 ║"
echo "╠══════════════════════════════════════════╣"
echo "║  Total requests:  ${TOTAL}"
echo "║  Successful:      ${SUCCESS}"
echo "║  Failed:          ${FAILED}"
echo "║  Total time:      ${ELAPSED_MS}ms"
echo "║  Requests/sec:    ${RPS}"
echo "║  Avg response:    ${AVG_TIME}s"
echo "║  P95 response:    ${P95_TIME}s"
echo "╚══════════════════════════════════════════╝"

# Cleanup
rm -f /tmp/himadri_results.txt
