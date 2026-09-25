# Third-party notice

`vendor/` holds Microsoft's XPress9 compression library (Copyright (c)
Microsoft Corporation, licensed under the MIT License — see the header of each
source file). The files were copied unmodified from
<https://github.com/Hugoberry/xpress9-python> at commit
`ff81bd9f93601650243a2695d1ad5940c7fbadd0` (`include/` and `src/`, without
that project's `Xpress9Wrapper` shim); `vendor/LICENSE` is that repository's
MIT license.

`csrc/ripbi_xpress9.c` is ripbi's own shim over the library's public API. It
replaces the upstream wrapper so that nothing ever writes to stdout or stderr.
