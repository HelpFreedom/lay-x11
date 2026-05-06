# Two-word model evaluation

Cases: 3118

| Layer             | OK        | Accuracy | Mean ms | p95 ms |
|-------------------|-----------|----------|---------|--------|
| consensus_arbiter | 3115/3118 | 99.9%    | 0.9     | 0.2    |

## consensus_arbiter

Bad cases: 3

| ID   | Category            | Typed    | Expected | Output   | Detail                                                                                                                                       |
|------|---------------------|----------|----------|----------|----------------------------------------------------------------------------------------------------------------------------------------------|
| 3091 | brand_single_letter | GitHub Н | GitHub Н | GitHub Y | consensus model_agree tiny2=(tiny2:flip_last model=-6.007 layout=0.60 n=2) sage=(sage:flip_last stability=1.000 normalized='GitHub Y.' n=2)  |
| 3099 | brand_single_letter | Qwen Н   | Qwen Н   | Qwen Y   | consensus model_agree tiny2=(tiny2:flip_last model=-10.931 layout=0.60 n=2) sage=(sage:flip_last stability=1.000 normalized='Qwen Y.' n=2)   |
| 3101 | brand_single_letter | BitNet Н | BitNet Н | BitNet Y | consensus model_agree tiny2=(tiny2:flip_last model=-10.676 layout=0.60 n=2) sage=(sage:flip_last stability=1.000 normalized='BitNet Y.' n=2) |
