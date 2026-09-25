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
