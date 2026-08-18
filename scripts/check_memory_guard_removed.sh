#!/usr/bin/env bash
# Closing-commit assertion for Phase 6 / Requirement 17.7.
# Verifies the Phase 4 stop-gap helpers have been removed and no
# call sites have re-emerged.
set -euo pipefail
if grep -rn "tactical_unload" src-tauri/src/ 2>/dev/null; then
  echo "FAIL: tactical_unload references found in src-tauri/src/"
  exit 1
fi
if grep -rn "memory_guard" src-tauri/src/ 2>/dev/null; then
  echo "FAIL: memory_guard references found in src-tauri/src/"
  exit 1
fi
echo "OK: Phase 4 stop-gap helpers fully removed."
