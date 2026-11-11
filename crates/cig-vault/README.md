# cig-vault

The vault is a small store that runs as its own account (`_cigtin`). cig hands
it the tin's objects over a local socket, and nothing that runs as you can
delete them: the protocol has no delete, and the store belongs to the vault's
account (mode 0700), so a write to it as you fails.

It offers `store`, `fetch`, `verify`, `has`, `list` and `gc`. Objects are named
the way the tin names them: a plain object or a chunk by the SHA-256 of its
bytes (`o`), a chunked file's recipe by the SHA-256 of the whole file (`r`), a
sealed object by its random id (`s`). One tin per user, told apart by the
peer's uid (`SO_PEERCRED`, `getpeereid`).

`gc` over the socket only reports what no live run names (the client sends the
live set). Removing is `cig-vault gc --apply`, run as the vault's account
through a sudo rule, never over the socket.

The wire protocol is in `src/lib.rs`. Unix only today.
