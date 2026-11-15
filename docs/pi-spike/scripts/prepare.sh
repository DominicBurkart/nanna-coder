#!/bin/sh
set -e
R=/var/tmp/nanna-pi-spike/nanna
rm -rf $R; mkdir -p $R/pi/extensions $R/work
cp /var/tmp/nanna-pi-spike/scripts/nanna-capabilities.ts $R/pi/extensions/nanna-capabilities.ts
cat > $R/pi/models.json <<'JSON'
{"providers":{"nanna":{"baseUrl":"http://127.0.0.1:18080/v1","api":"openai-completions","apiKey":"nanna","models":[{"id":"gemma4:e4b"}]}}}
JSON
echo "Be careful. NANNA-SYSTEM-MARKER" > $R/pi/SYSTEM.md
cat > $R/pi/capabilities.json <<'JSON'
[{"name":"read_file","description":"read_file tool","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]},"effect":"none"}]
JSON
