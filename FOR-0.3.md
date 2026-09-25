# Incompatible changes planned for 0.3

Planned changes that would break compatibility, and thus have to wait for the
0.3 release.

## Make `Storage::settle_writes()` required

`Storage::settle_writes()` currently has a default implementation that does
nothing, because there is nothing we can do that would generally be correct; but
we need to provide a default implementation to keep compatibility.

Storage drivers absolutely must consider whether to implement this or leave it
empty, so this method must lose its default implementation, because if a driver
uses the default implementation while dropped write, write-zeroes, or discard
futures can still modify data, that can lead to data corruption.

## Make `FormatDriverInstance::try_readv_special()` required

`FormatDriverInstance::try_readv_special()` returns the unhandled tail of the
buffer if the range turns out not to be special (anymore) from some point on,
e.g. because a concurrent write has changed its mapping since `FormatAccess`
looked it up.  `FormatAccess` then reads that tail according to its current
mapping.

It currently has a default implementation on top of `readv_special()`, which
never returns a tail (and so leads to errors in the case described above).
Make `try_readv_special()` required, and give `readv_special()` a default
implementation on top of `try_readv_special()`.
