# anchor_029_to_030

`old.json` is a 0.30-format IDL with its discriminators left out, as some hand-written or
stripped IDLs are; `new.json` writes them. A parser that reads discriminators from the IDL
alone sees a new 8-byte scheme. (A real legacy IDL, with `name` at the top level, gets its
discriminators computed and would not trip this.)

Expected: Breaking (discriminator length 0 to 8, for an instruction and an account), exit 1.
