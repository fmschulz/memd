# Cache Key Scope Fixture

This fixture models a feature flag service with tenant-scoped authorization and
a shared read-through cache. The failures could look like authorization,
defaults, or rollout logic, but the contract bug is that the cache key omits
the tenant.

Fix the implementation without changing public function names.

Run:

```bash
python3 -m unittest -q
```
