# Two-word model evaluation

Cases: 20

| Layer | OK   | Accuracy | Mean ms | p95 ms |
|-------|------|----------|---------|--------|
| sage  | 0/20 | 0.0%     | 129.9   | 177.4  |

## sage

Bad cases: 20

| ID  | Category          | Typed            | Expected         | Output            | Detail   |
|-----|-------------------|------------------|------------------|-------------------|----------|
| 001 | en_left_ru_target | good ntrcn       | good текст       | Good. ntrcn.      | generate |
| 002 | en_left_ru_target | good ckjdf       | good слова       | Good ckjdf.       | generate |
| 003 | en_left_ru_target | good ghbdtn      | good привет      | Good ghbdtn.      | generate |
| 004 | en_left_ru_target | good vbh         | good мир         | Good vbh.         | generate |
| 005 | en_left_ru_target | good ghjcnj      | good просто      | Good ghjcnj.      | generate |
| 006 | en_left_ru_target | good ghjcnjcnm   | good простость   | Good ghjcnjcnm.   | generate |
| 007 | en_left_ru_target | good ntcn        | good тест        | Good. ntcn.       | generate |
| 008 | en_left_ru_target | good rjl         | good код         | Good rjl.         | generate |
| 009 | en_left_ru_target | good hf,jnf      | good работа      | Good hf,jnf.      | generate |
| 010 | en_left_ru_target | good vj;yj       | good можно       | Good vjpyj.       | generate |
| 011 | en_left_ru_target | test ntrcn       | test текст       | Test. ntrcn.      | generate |
| 012 | en_left_ru_target | test ckjdf       | test слова       | Test ckjdf.       | generate |
| 013 | en_left_ru_target | test ghbdtn      | test привет      | Test ghbdtn.      | generate |
| 014 | en_left_ru_target | test vbh         | test мир         | Test vbh.         | generate |
| 015 | en_left_ru_target | test ghjcnj      | test просто      | Test ghjcnj.      | generate |
| 016 | en_left_ru_target | test ntcn        | test тест        | Test. ntcn.       | generate |
| 017 | en_left_ru_target | test hf,jnf      | test работа      | Test hf, jnf.     | generate |
| 018 | en_left_ru_target | test vj;yj       | test можно       | Test vjpyj.       | generate |
| 019 | en_left_ru_target | live lbcnhb,enbd | live дистрибутив | Live lbcnhb,enbd. | generate |
| 020 | en_left_ru_target | live ljvf        | live дома        | Live. live.       | generate |
