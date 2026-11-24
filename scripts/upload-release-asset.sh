#!/usr/bin/env bash
set -euo pipefail
: "${FORGE_API:?}" "${FORGE_REPO:?}" "${FORGE_TOKEN:?}" "${RELEASE_TAG:?}"

AUTH="Authorization: token $FORGE_TOKEN"
API=$FORGE_API/repos/$FORGE_REPO/releases

release_id() {
  curl -fsS -H "$AUTH" "$API/tags/$RELEASE_TAG" 2>/dev/null | sed -n 's/^{"id":\([0-9]*\),.*/\1/p'
}

ID=$(release_id || true)
if [ -z "$ID" ]; then
  PRERELEASE=false
  case $RELEASE_TAG in *-*) PRERELEASE=true ;; esac
  curl -fsS -H "$AUTH" -H 'Content-Type: application/json' -o /dev/null \
    -d "{\"tag_name\":\"$RELEASE_TAG\",\"name\":\"$RELEASE_TAG\",\"prerelease\":$PRERELEASE}" "$API" ||
    true
  ID=$(release_id)
fi

for f in "$@"; do
  name=$(basename "$f")
  existing=$(curl -fsS -H "$AUTH" "$API/$ID/assets" |
    tr '}' '\n' | sed -n "s/.*\"id\":\([0-9]*\),\"name\":\"$name\".*/\1/p")
  [ -z "$existing" ] || curl -fsS -X DELETE -H "$AUTH" "$API/$ID/assets/$existing"
  curl -fsS -H "$AUTH" -F "attachment=@$f" "$API/$ID/assets?name=$name" -o /dev/null
  echo "uploaded $name"
done
