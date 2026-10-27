# fetchloom

put the exact data a project needs onto any machine with one command.

```
fetchloom add zenodo:3608990
fetchloom add https://drive.google.com/file/d/1AbCdEf/view
fetchloom sync
```

early development. nothing to install yet.

## design

| doc | covers |
|---|---|
| [vision](docs/vision.md) | why fetchloom exists, who it serves first, what it will not build |
| [scope](docs/scope.md) | exactly what v0.1 includes, and how it is tested |
| [cli](docs/cli.md) | how it looks and talks |
| [architecture](docs/architecture.md) | how it works inside |
| [formats](docs/formats.md) | `data.toml`, `data.lock`, the cache and the JSON output |
| [decisions](docs/decisions.md) | the choices that shape the code, and why |
| [performance](docs/performance.md) | budgets, counters and profiling |
| [engineering](docs/engineering.md) | code, tests, CI and releases |
| [style](docs/style.md) | how we write |
| [plan](docs/plan.md) | the order v0.1 is built in |

## license

apache-2.0. see [LICENSE](LICENSE) and [NOTICE](NOTICE).
