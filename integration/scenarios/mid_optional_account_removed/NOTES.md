# mid_optional_account_removed

The optional `referral` account sits in the middle of the list. Removing it moves every
later account up one position, so a parser that reads accounts by index binds them wrong.
Removing an optional account from the tail would only be Dangerous.

Expected: Breaking (accounts changed at fixed positions, account removed), exit 1.
