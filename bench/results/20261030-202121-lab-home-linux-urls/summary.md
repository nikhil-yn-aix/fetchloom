# lab-home-linux-urls

## many-1k

| tool | phase | correct | wall median s | wall range s | x floor | cpu median s | peak MB | disk MB | paired with fetchloom |
|---|---|---|---|---|---|---|---|---|---|
| aria2c-j16 | first | 3 of 3 | 8.94 | 8.90 to 8.94 |  | 1.43 | 51 | 102 | 0.94 (0.94 to 0.95), fetchloom slower |
| curl-parallel | first | 3 of 3 | 9.02 | 9.01 to 9.03 |  | 1.99 | 35 | 102 | 0.95 (0.95 to 0.96), fetchloom slower |
| fetchloom | first | 3 of 3 | 9.46 | 9.39 to 9.48 |  | 2.80 | 34 | 103 |  |
| fetchloom | second | 3 of 3 | 0.30 | 0.30 to 0.31 |  | 0.29 | 30 | 104 |  |
