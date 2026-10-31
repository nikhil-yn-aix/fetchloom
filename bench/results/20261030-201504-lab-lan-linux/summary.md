# lab-lan-linux

## file-1g

| tool | phase | correct | wall median s | wall range s | x floor | cpu median s | peak MB | disk MB | paired with fetchloom |
|---|---|---|---|---|---|---|---|---|---|
| aria2c | first | 3 of 3 | 1.90 | 1.66 to 2.67 | 0.57 | 1.62 | 20 | 1074 | 0.53 (0.48 to 0.55), fetchloom slower |
| aria2c-tuned | first | 3 of 3 | 2.19 | 1.80 to 2.37 | 0.65 | 1.91 | 35 | 1074 | 0.57 (0.45 to 0.59), fetchloom slower |
| curl-sha256 | first | 3 of 3 | 3.36 | 2.68 to 3.70 |  | 3.42 | 40 | 1074 | 0.76 (0.67 to 1.07), no clear difference |
| datalad | first | 3 of 3 | 5.51 | 4.21 to 5.52 | 1.64 | 5.25 | 146 | 1074 | 1.34 (1.14 to 1.38), no clear difference |
| dvc | first | 3 of 3 | 6.13 | 5.86 to 8.04 | 1.82 | 6.19 | 98 | 2148 | 1.95 (1.21 to 2.01), fetchloom faster |
| dvc | second | 3 of 3 | 2.56 | 2.46 to 3.26 |  | 2.55 | 84 | 3221 | 272.78 (256.20 to 325.80), fetchloom faster |
| fetchloom | first | 3 of 3 | 4.00 | 3.15 to 4.86 | 1.19 | 6.12 | 17 | 1074 |  |
| fetchloom | second | 3 of 3 | 0.01 | 0.01 to 0.01 |  | 0.01 | 0 | 1074 |  |
| pooch | first | 3 of 3 | 8.83 | 8.20 to 9.09 | 2.62 | 9.15 | 42 | 1074 | 2.21 (1.69 to 2.89), fetchloom faster |
| pooch | second | 3 of 3 | 0.91 | 0.86 to 0.92 |  | 0.95 | 26 | 1074 | 91.70 (91.30 to 95.78), fetchloom faster |

## file-100m

| tool | phase | correct | wall median s | wall range s | x floor | cpu median s | peak MB | disk MB | paired with fetchloom |
|---|---|---|---|---|---|---|---|---|---|
| aria2c | first | 3 of 3 | 0.23 | 0.22 to 0.25 | 0.56 | 0.18 | 20 | 105 | 0.72 (0.63 to 0.73), fetchloom slower |
| aria2c-tuned | first | 3 of 3 | 0.27 | 0.26 to 0.28 | 0.68 | 0.20 | 35 | 105 | 0.79 (0.75 to 0.88), fetchloom slower |
| curl-sha256 | first | 3 of 3 | 0.41 | 0.34 to 0.41 |  | 0.41 | 41 | 105 | 1.16 (1.10 to 1.17), no clear difference |
| datalad | first | 3 of 3 | 1.71 | 1.54 to 2.96 | 4.21 | 1.78 | 150 | 105 | 4.95 (4.84 to 8.56), fetchloom faster |
| dvc | first | 3 of 3 | 2.10 | 2.01 to 5.60 | 5.19 | 2.00 | 98 | 210 | 6.49 (5.95 to 16.17), fetchloom faster |
| dvc | second | 3 of 3 | 1.66 | 1.58 to 1.92 |  | 1.52 | 85 | 315 | 166.10 (157.60 to 192.40), fetchloom faster |
| fetchloom | first | 3 of 3 | 0.35 | 0.31 to 0.35 | 0.85 | 0.49 | 13 | 105 |  |
| fetchloom | second | 3 of 3 | 0.01 | 0.01 to 0.01 |  | 0.01 | 0 | 105 |  |
| pooch | first | 3 of 3 | 1.13 | 0.94 to 1.17 | 2.78 | 1.20 | 42 | 105 | 3.19 (3.03 to 3.37), fetchloom faster |
| pooch | second | 3 of 3 | 0.20 | 0.19 to 0.24 |  | 0.22 | 26 | 105 | 20.30 (19.10 to 24.30), fetchloom faster |

## file-10m

| tool | phase | correct | wall median s | wall range s | x floor | cpu median s | peak MB | disk MB | paired with fetchloom |
|---|---|---|---|---|---|---|---|---|---|
| aria2c | first | 3 of 3 | 0.05 | 0.05 to 0.06 | 0.60 | 0.03 | 19 | 10 | 0.96 (0.85 to 1.15) |
| aria2c-tuned | first | 3 of 3 | 0.07 | 0.07 to 0.08 | 0.78 | 0.05 | 28 | 10 | 1.18 (1.11 to 1.44) |
| curl-sha256 | first | 3 of 3 | 0.09 | 0.09 to 0.10 |  | 0.09 | 30 | 10 | 1.60 (1.57 to 1.68) |
| datalad | first | 3 of 3 | 1.45 | 1.35 to 1.45 | 16.45 | 1.34 | 164 | 11 | 24.51 (23.42 to 26.81) |
| dvc | first | 3 of 3 | 1.59 | 1.56 to 1.75 | 18.10 | 1.48 | 98 | 21 | 28.98 (25.69 to 31.87) |
| dvc | second | 3 of 3 | 1.46 | 1.44 to 1.62 |  | 1.32 | 85 | 32 | 179.50 (162.56 to 180.11) |
| fetchloom | first | 3 of 3 | 0.06 | 0.05 to 0.06 | 0.62 | 0.06 | 11 | 10 |  |
| fetchloom | second | 3 of 3 | 0.01 | 0.01 to 0.01 |  | 0.01 | 0 | 10 |  |
| pooch | first | 3 of 3 | 0.29 | 0.28 to 0.29 | 3.30 | 0.29 | 42 | 10 | 5.22 (4.69 to 5.27) |
| pooch | second | 3 of 3 | 0.12 | 0.11 to 0.12 |  | 0.12 | 27 | 10 | 12.89 (12.67 to 14.62) |

## files-10k-zip

| tool | phase | correct | wall median s | wall range s | x floor | cpu median s | peak MB | disk MB | paired with fetchloom |
|---|---|---|---|---|---|---|---|---|---|
| curl-bsdtar | first | 3 of 3 | 0.66 | 0.60 to 1.12 |  | 0.66 | 41 | 83 | 0.69 (0.57 to 1.43), no clear difference |
| fetchloom | first | 3 of 3 | 0.87 | 0.78 to 1.16 | 1.33 | 2.49 | 22 | 42 |  |
| fetchloom | second | 3 of 3 | 0.20 | 0.20 to 0.22 |  | 0.22 | 15 | 42 |  |
| pooch | first | 3 of 3 | 2.21 | 1.88 to 2.27 | 3.37 | 2.27 | 56 | 83 | 2.15 (1.91 to 2.90), fetchloom faster |
| pooch | second | 3 of 3 | 0.86 | 0.84 to 0.96 |  | 0.93 | 42 | 83 | 4.24 (3.77 to 4.86), fetchloom faster |

## files-10k-tar-zst

| tool | phase | correct | wall median s | wall range s | x floor | cpu median s | peak MB | disk MB | paired with fetchloom |
|---|---|---|---|---|---|---|---|---|---|
| curl-bsdtar | first | 3 of 3 | 0.59 | 0.59 to 0.61 |  | 0.61 | 38 | 82 | 0.78 (0.72 to 0.83) |
| fetchloom | first | 3 of 3 | 0.76 | 0.73 to 0.82 | 1.28 | 2.53 | 24 | 42 |  |
| fetchloom | second | 3 of 3 | 0.21 | 0.20 to 0.22 |  | 0.22 | 15 | 42 |  |
