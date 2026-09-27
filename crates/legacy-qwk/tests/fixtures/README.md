`python-deflate.rep` is a deterministic independent ZIP fixture written with
Python's standard-library `zipfile.ZipFile` and `ZIP_DEFLATED`, not RabbitHole's
ZIP writer. Its single `WARREN.MSG` entry uses a 2026-09-26 12:30:00 timestamp.

The member was constructed directly from QWK byte offsets: a 128-byte `WARREN`
header padded with spaces, one 128-byte reply header (`ALL`, `ALICE`, subject
`Independent fixture`, block count `2`, conference `1` in offsets 125..127),
and a 128-byte space-padded `From Python zipfile.` body. This checks a common
external ZIP implementation, not interoperability with a real offline reader.
