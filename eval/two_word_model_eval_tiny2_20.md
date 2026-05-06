# Two-word model evaluation

Cases: 20

| Layer          | OK   | Accuracy | Mean ms | p95 ms |
|----------------|------|----------|---------|--------|
| tiny2_pseudoll | 0/20 | 0.0%     | 79.0    | 124.1  |

## tiny2_pseudoll

Bad cases: 20

| ID  | Category          | Typed            | Expected         | Output           | Detail                  |
|-----|-------------------|------------------|------------------|------------------|-------------------------|
| 001 | en_left_ru_target | good ntrcn       | good текст       | пщщв ntrcn       | flip_first score=-7.033 |
| 002 | en_left_ru_target | good ckjdf       | good слова       | пщщв ckjdf       | flip_first score=-5.308 |
| 003 | en_left_ru_target | good ghbdtn      | good привет      | пщщв ghbdtn      | flip_first score=-6.151 |
| 004 | en_left_ru_target | good vbh         | good мир         | пщщв мир         | flip_all score=-7.060   |
| 005 | en_left_ru_target | good ghjcnj      | good просто      | пщщв ghjcnj      | flip_first score=-5.508 |
| 006 | en_left_ru_target | good ghjcnjcnm   | good простость   | пщщв ghjcnjcnm   | flip_first score=-4.484 |
| 007 | en_left_ru_target | good ntcn        | good тест        | пщщв ntcn        | flip_first score=-6.221 |
| 008 | en_left_ru_target | good rjl         | good код         | пщщв rjl         | flip_first score=-5.085 |
| 009 | en_left_ru_target | good hf,jnf      | good работа      | пщщв hf,jnf      | flip_first score=-4.844 |
| 010 | en_left_ru_target | good vj;yj       | good можно       | пщщв vj;yj       | flip_first score=-4.989 |
| 011 | en_left_ru_target | test ntrcn       | test текст       | test ntrcn       | keep score=-11.119      |
| 012 | en_left_ru_target | test ckjdf       | test слова       | test ckjdf       | keep score=-6.735       |
| 013 | en_left_ru_target | test ghbdtn      | test привет      | test ghbdtn      | keep score=-8.558       |
| 014 | en_left_ru_target | test vbh         | test мир         | test vbh         | keep score=-11.095      |
| 015 | en_left_ru_target | test ghjcnj      | test просто      | test ghjcnj      | keep score=-7.246       |
| 016 | en_left_ru_target | test ntcn        | test тест        | test ntcn        | keep score=-8.993       |
| 017 | en_left_ru_target | test hf,jnf      | test работа      | test hf,jnf      | keep score=-5.938       |
| 018 | en_left_ru_target | test vj;yj       | test можно       | test vj;yj       | keep score=-6.341       |
| 019 | en_left_ru_target | live lbcnhb,enbd | live дистрибутив | дшму lbcnhb,enbd | flip_first score=-6.985 |
| 020 | en_left_ru_target | live ljvf        | live дома        | дшму ljvf        | flip_first score=-6.656 |
