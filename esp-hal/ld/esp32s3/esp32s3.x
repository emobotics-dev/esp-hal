INCLUDE exception.x

SECTIONS {
  /* NOLOAD, for lld, which links the S3 since the esp 1.100 toolchain carries no GNU
   * ld; lld does not parse GNU's READONLY. Under lld this section comes out SHT_NOBITS
   * with SHF_ALLOC only, in a read-only LOAD segment of its own, and .text stays R E —
   * measured on the CoreS3 evcc.
   *
   * Under GNU ld, NOLOAD gets SHF_WRITE, the section shares .text's segment, and ld warns
   * that the segment is RWX: why this said READONLY while the S3 linked with GNU ld. A
   * build still linking with it sees that warning again; `-D warnings` does not catch it
   * (the `linker_messages` lint ignores it), and the image is unchanged.
   */
  .rotext_dummy (NOLOAD) :
  {
    /* This dummy section represents the .rodata section within ROTEXT.
    * Since the same physical memory is mapped to both DROM and IROM,
    * we need to make sure the .rodata and .text sections don't overlap.
    * We skip the amount of memory taken by .rodata* in .text
    */

    /* Start at the same alignment constraint than .flash.text */

    . = ALIGN(ALIGNOF(.rodata));
    . = ALIGN(ALIGNOF(.rodata.wifi));

    /* Create an empty gap as big as .text section */

    . = . + SIZEOF(.flash.appdesc);
    . = . + SIZEOF(.rodata);
    . = . + SIZEOF(.rodata.wifi);

    /* Prepare the alignment of the section above. Few bytes (0x20) must be
     * added for the mapping header.
     */

    . = ALIGN(0x10000) + 0x20;
    _rotext_reserved_start = .;
  } > ROTEXT
}
INSERT BEFORE .text;

/* Similar to .rotext_dummy this represents .rwtext but in .data */
SECTIONS {
  .rwdata_dummy (NOLOAD) : ALIGN(4)
  {
    . = . + SIZEOF(.rwtext) + SIZEOF(.rwtext.wifi) + SIZEOF(.vectors);
  } > RWDATA
}
INSERT BEFORE .data;

/* Shared sections - ordering matters */
SECTIONS {
  INCLUDE "rwtext.x"
  INCLUDE "rwdata.x"
}
INCLUDE "rodata.x"
INCLUDE "text.x"
INCLUDE "rtc_fast.x"
INCLUDE "rtc_slow.x"
INCLUDE "stack.x"
INCLUDE "dram2.x"
INCLUDE "dcache_reclaimed.x"
INCLUDE "metadata.x"
INCLUDE "eh_frame.x"
/* End of Shared sections */
