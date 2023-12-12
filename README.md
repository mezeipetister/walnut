<image src="logo.svg" width="200" alt="Walnut file system">

# Walnut
Walnut experimental virtual file system

*This project is in alpha status. Don't use it in production.*

Key features:

- great IO performance
- fast and secure data encryption
- directory management
- easy api
- CLI
- auto data scaling (only upwards)

*inspired by:*
- https://en.wikipedia.org/wiki/Ext4
- https://ext4.wiki.kernel.org/index.php/Ext4_Disk_Layout
- https://github.com/carlosgaldino/gotenksfs

## FS design:

<image src="docs/design_schema.svg" width="700" alt="Walnut file system">

## CLI

*work in progress*

Init walnut fs:

```bash
wlnt FS_PATH SECRET init
```

Adding file to walnut fs:

```bash
wlnt FS_PATH SECRET add FROM_PATH PATH FILENAME
```

Exporting from walnut fs:

```bash
wlnt FS_PATH SECRET export PATH FILENAME EXPORT_PATH
```

## Encryption

Walnut uses XOR (1) operation at bit level. Creating a 4kib lookup table from the given secret, and performing XOR between data and lookup table bits. We use lookup table to increase performance.

(1) https://en.wikipedia.org/wiki/Exclusive_or