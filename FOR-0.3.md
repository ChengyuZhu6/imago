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

## Require `{prepare,commit}_data_mapping()`, deprecate `ensure_data_mapping()`

`FormatDriverInstance::{prepare,commit}_data_mapping()` allow writing the data
for a new mapping before that mapping becomes visible, so readers never see what
the mapped location contained before.

To keep compatibility:
- They currently have default implementations via `ensure_data_mapping()`, which
  make the mapping visible immediately, i.e. do not provide that guarantee.
- And `ensure_data_mapping()` in turn has a default implementation on top of
  both, so that it can still be used with drivers providing the new variants.

For correctness’s sake, format drivers must consider what their mappings expose
before they are written, so `{prepare,commit}_data_mapping()` must lose their
default implementations.

In turn, `ensure_data_mapping()` must be wholly deprecated, and the driver
implementation removed.

## Deprecate the `sync-wrappers` feature

`sync-wrappers` provides synchronous wrappers around the `async` interface,
which run the `async` functions in a `tokio` runtime they build.  The `sync`
feature provides native synchronous methods without that overhead, and the
feature documentation already announces that `sync-wrappers` is planned to be
deprecated.

The wrappers also duplicate the `async` interface’s documentation, which has to
be kept in sync by hand (e.g. `SyncFormatAccess` does not mention that requests
beyond the disk size return an error, unlike `FormatAccess`).
