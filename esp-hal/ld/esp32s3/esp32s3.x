INCLUDE exception.x

SECTIONS {
  /* READONLY, not NOLOAD: a NOLOAD output section gets SHF_WRITE, and this one
   * shares a LOAD segment with .text (SHF_EXECINSTR), so the segment came out
   * RWX — which ld reports as "has a LOAD segment with RWX permissions". That
   * warning survives a -D warnings build, because the `linker_messages` lint
   * documents that it ignores it.
   *
   * READONLY keeps the section SHT_NOBITS here (it has no content statements,
   * only a `.` advance), so the image does not grow. Measured, not assumed —
   * the combined `READONLY (TYPE = SHT_NOBITS)` form parses but silently drops
   * the readonly half, and plain `TYPE =` does too; only bare READONLY works.
   */
  .rotext_dummy (READONLY) :
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
INCLUDE "metadata.x"
INCLUDE "eh_frame.x"
/* End of Shared sections */
