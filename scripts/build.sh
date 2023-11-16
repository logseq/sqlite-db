#!/bin/bash

set -ex
set -o pipefail

wasm-pack build --target web --release

# Convert package.json name to scopled

sed -i '' 's/"name": "logseq-sqlite",/"name": "@logseq\/sqlite",/g' pkg/package.json


# Convert import.meta.url to location.origin

sed -i '' 's/import.meta.url/location.origin/g' pkg/logseq_sqlite.js


echo ".wasm file should be copied too!!!"
