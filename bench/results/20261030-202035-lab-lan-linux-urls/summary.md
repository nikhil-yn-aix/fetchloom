# lab-lan-linux-urls

## many-1k

| tool | phase | correct | wall median s | wall range s | x floor | cpu median s | peak MB | disk MB | paired with fetchloom |
|---|---|---|---|---|---|---|---|---|---|
| aria2c-j16 | first | 3 of 3 | 8.44 | 8.24 to 8.60 |  | 1.18 | 51 | 102 | 2.34 (2.31 to 2.44), fetchloom faster |
| curl-parallel | first | 3 of 3 | 0.30 | 0.29 to 0.33 |  | 0.31 | 42 | 102 | 0.09 (0.08 to 0.09), fetchloom slower |
| fetchloom | first | 3 of 3 | 3.53 | 3.52 to 3.66 |  | 2.16 | 35 | 103 |  |
| fetchloom | second | 3 of 3 | 0.31 | 0.29 to 0.32 |  | 0.29 | 30 | 104 |  |
