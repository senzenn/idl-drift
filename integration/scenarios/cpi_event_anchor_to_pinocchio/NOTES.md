# cpi_event_anchor_to_pinocchio

Self-CPI events sit inside an envelope: Anchor's 8-byte event tag, or the 1-byte wrap many
Pinocchio programs use. Porting the program from one to the other moves where the payload
starts.

Expected: Breaking (envelope discriminator length 8 to 1, payload offset 8 to 1), exit 1.
