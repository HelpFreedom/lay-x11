# Two-word model evaluation

Cases: 3118

| Layer         | OK        | Accuracy | Mean ms | p95 ms |
|---------------|-----------|----------|---------|--------|
| tiny2_arbiter | 3108/3118 | 99.7%    | 0.2     | 0.2    |

## tiny2_arbiter

Bad cases: 10

| ID   | Category            | Typed     | Expected  | Output    | Detail                                        |
|------|---------------------|-----------|-----------|-----------|-----------------------------------------------|
| 3089 | brand_single_letter | AmoCRM Н  | AmoCRM Н  | AmoCRM Y  | tiny2:flip_last model=-6.406 layout=0.20 n=2  |
| 3091 | brand_single_letter | GitHub Н  | GitHub Н  | GitHub Y  | tiny2:flip_last model=-6.007 layout=0.60 n=2  |
| 3093 | brand_single_letter | GNOME Н   | GNOME Н   | GNOME Y   | tiny2:flip_last model=-6.542 layout=0.60 n=2  |
| 3095 | brand_single_letter | Wayland Н | Wayland Н | Wayland Y | tiny2:flip_last model=-8.419 layout=0.60 n=2  |
| 3097 | brand_single_letter | Rust Н    | Rust Н    | Rust Y    | tiny2:flip_last model=-7.816 layout=0.60 n=2  |
| 3099 | brand_single_letter | Qwen Н    | Qwen Н    | Qwen Y    | tiny2:flip_last model=-10.931 layout=0.60 n=2 |
| 3101 | brand_single_letter | BitNet Н  | BitNet Н  | BitNet Y  | tiny2:flip_last model=-10.676 layout=0.60 n=2 |
| 3103 | brand_single_letter | API Н     | API Н     | API Y     | tiny2:flip_last model=-13.576 layout=0.60 n=2 |
| 3105 | brand_single_letter | CPU Н     | CPU Н     | CPU Y     | tiny2:flip_last model=-13.728 layout=0.60 n=2 |
| 3107 | brand_single_letter | LLM Н     | LLM Н     | LLM Y     | tiny2:flip_last model=-8.797 layout=0.60 n=2  |
