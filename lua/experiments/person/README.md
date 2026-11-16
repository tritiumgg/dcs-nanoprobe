# Person tests

Snippets for the checks only a person at a client can judge: whether a sound is heard, a
picture shows, what a client sees after a damage kill, whether a radio menu item picked by
hand runs its callback, and the player name the server sees. Run each with
`cargo xtask eval <server> <state> <file>` on a server running a `--dev` build and a
mission packed with `cargo xtask miz <mission> <out> --sound-picture`, with one person in
an aircraft slot. Each file says its state and what the person looks for.
