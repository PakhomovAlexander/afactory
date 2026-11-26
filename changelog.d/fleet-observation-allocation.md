- Project origin reads bound SQLite snapshot allocation on the destination before copying,
  including ext4 filesystems with allocation clusters larger than their filesystem blocks.
  Destinations where the reader cannot establish a bound fail closed.
