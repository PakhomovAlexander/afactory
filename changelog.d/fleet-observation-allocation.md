- Project origin reads bound SQLite snapshot allocation on the destination before copying,
  including ext4 filesystems with allocation clusters larger than their filesystem blocks.
  On the ext filesystem family, the reservation includes possible indirect and extent-tree
  blocks for the database, WAL and rebuilt WAL index.
  Destinations where the reader cannot establish a bound fail closed.
