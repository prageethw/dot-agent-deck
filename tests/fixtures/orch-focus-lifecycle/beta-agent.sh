#!/bin/sh
# PRD #311 `tabs/orchestration/006` placeholder — overwritten at test runtime
# (`tests/e2e_orchestration_pane_column.rs::write_beta_agent`) with the
# absolute path of the freshly built test binary baked in, so it never depends
# on PATH resolving to a dev machine's separately installed `dot-agent-deck`.
printf 'BETA_ROLE_SENTINEL\n'
sleep 600
