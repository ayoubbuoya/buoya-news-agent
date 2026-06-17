#!/usr/bin/env bash
# Launcher for the MCP server. Runs from the repo root so the binary finds
# config.default.toml and writes data/buoya.db there, regardless of which
# client (Claude Code / Claude Desktop) spawns it.
cd "$(dirname "$0")" || exit 1
exec ./target/release/buoya-news-mcp
