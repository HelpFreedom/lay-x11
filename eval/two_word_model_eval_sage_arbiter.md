# Two-word model evaluation

Cases: 3118

| Layer        | OK        | Accuracy | Mean ms | p95 ms |
|--------------|-----------|----------|---------|--------|
| sage_arbiter | 3115/3118 | 99.9%    | 0.8     | 0.2    |

## sage_arbiter

Bad cases: 3

| ID   | Category            | Typed    | Expected | Output   | Detail                                                    |
|------|---------------------|----------|----------|----------|-----------------------------------------------------------|
| 3091 | brand_single_letter | GitHub Н | GitHub Н | GitHub Y | sage:flip_last stability=1.000 normalized='GitHub Y.' n=2 |
| 3099 | brand_single_letter | Qwen Н   | Qwen Н   | Qwen Y   | sage:flip_last stability=1.000 normalized='Qwen Y.' n=2   |
| 3101 | brand_single_letter | BitNet Н | BitNet Н | BitNet Y | sage:flip_last stability=1.000 normalized='BitNet Y.' n=2 |
