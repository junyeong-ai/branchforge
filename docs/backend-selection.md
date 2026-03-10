# Backend Selection Guide

Use this guide to choose a persistence backend for `claude-agent-rs`.

## PostgreSQL

Choose PostgreSQL when you want the safest default production backend.

Strengths:

- strongest operational familiarity
- good graph-first session durability
- identity-aware persistence
- Docker-backed integration coverage in this repository

Recommended role:

- default production backend

## Redis

Choose Redis when you want lightweight persistence, queue semantics, or support infrastructure.

Strengths:

- good fit for queue and cache behavior
- simple operational profile for support workloads
- Docker-backed integration coverage in this repository

Recommended role:

- support backend
- cache or queue oriented backend

## JSONL

Choose JSONL when you want local, portable, inspectable persistence.

Strengths:

- local debugging
- portable session artifacts
- easy inspection and export workflows

Recommended role:

- local development backend
- portable archive/debug backend

## Default Recommendation

- production default: PostgreSQL
- support/cache backend: Redis
- local/dev backend: JSONL
