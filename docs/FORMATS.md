# Format coverage

What each crate reads and writes, and what it leaves out.

## PNG (kinogaki-png)

See README.md: every color type and bit depth, Adam7, `tRNS`; 8/16-bit
writing. Not handled: color management chunks (samples are taken as sRGB),
APNG frames (only the default image is read).
