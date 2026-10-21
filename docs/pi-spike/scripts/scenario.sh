#!/bin/bash
NAME=$1; shift
S=/var/tmp/nanna-pi-spike
mkdir -p $S/logs/$NAME
/var/tmp/nanna-pi-spike/scripts/prepare.sh
[ -n "$PREP" ] && eval "$PREP"
podman run --rm --network none \
  -v $S:/spike:Z -v $S/inst:/inst:ro,Z -v $S/nanna:/nanna:Z \
  -e HOME=/nanna/work -e PI_CODING_AGENT_DIR=/nanna/pi -e NANNA_BROKER_SOCKET=/nanna/broker.sock -e NANNA_CAPABILITIES=/nanna/pi/capabilities.json \
  -e GATEWAY_LOG=/spike/logs/$NAME/gateway.jsonl -e BROKER_LOG=/spike/logs/$NAME/broker.jsonl -e RPC_OUT=/spike/logs/$NAME/rpc.jsonl \
  "$@" node:24-slim sh -c 'node /spike/scripts/gateway.js >/spike/logs/'$NAME'/gateway.out 2>&1 & node /spike/scripts/broker.js >/spike/logs/'$NAME'/broker.out 2>&1 & sleep 1; node /spike/scripts/run.js; sleep 1; ls -la /nanna/pi > /spike/logs/'$NAME'/agentdir.txt 2>&1'
echo "== $NAME done"; ls $S/logs/$NAME
