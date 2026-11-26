# `linear_array`

- **Linear arrays**: the source is copy 0; outputs `name_1..{count-1}`.
  Shrinking an array (or any op that makes fewer copies) drops the in-place
  material / hide / transform edits of the copies it no longer makes, with a
  `note:` listing them, instead of failing.
  `local: true` rotates `offset` by the source's rotation.
