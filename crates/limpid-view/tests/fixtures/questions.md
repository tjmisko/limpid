# Storage plan

We need somewhere to keep the session index, and the choice shapes
everything that follows.

## Questions

1. Which store should hold the index?
   - SQLite, one file per workspace
   - Postgres, shared
2. How long do we keep entries?
