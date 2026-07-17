#!/usr/bin/env bash
# Build all IronClaw thesis figures from TikZ sources into ../<name>.pdf
set -e
cd "$(dirname "$0")"
mkdir -p build
FIGS=("$@")
if [ ${#FIGS[@]} -eq 0 ]; then
  FIGS=(fig_architecture fig_dynamic_tool_flow fig_evaluation_pillars \
        fig_isolation_boundaries fig_microvm_lifecycle fig_protocol_envelope \
        fig_sandbox_comparison fig_threat_model)
fi
for f in "${FIGS[@]}"; do
  echo ">>> building $f"
  pdflatex -interaction=nonstopmode -halt-on-error \
    -output-directory=build "$f.tex" > "build/$f.pdflatex.log" 2>&1 || {
      echo "FAILED: $f (see build/$f.pdflatex.log)"; tail -n 25 "build/$f.pdflatex.log"; exit 1; }
  cp "build/$f.pdf" "../$f.pdf"
done
echo "OK: figures written to Figures/"
