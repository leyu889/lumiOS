ENTRY(_start)

SECTIONS
{
  . = 0x400000;

  .text : {
    *(.text._start)
    *(.text .text.*)
  }

  . = ALIGN(4096);
  .rodata : {
    *(.rodata .rodata.*)
  }

  . = ALIGN(4096);
  .data : {
    *(.data .data.*)
    /* 这几段以前是孤儿段：lld 会把它们单独排成**非页对齐**的 LOAD 段，
       而内核的 ELF 解析器只接受页对齐段 —— 于是 exec 返回 ENOEXEC（-8）。
       显式收进来，段数从 4 降到 3，且全部 4K 对齐。 */
    *(.got .got.*)
    *(.data.rel.ro .data.rel.ro.*)
    *(.sdata .sdata.*)
    *(.sdata2 .sdata2.*)
  }

  .bss : {
    *(.bss .bss.*)
    *(.sbss .sbss.*)
    *(COMMON)
  }

  /DISCARD/ : {
    *(.eh_frame)
    *(.eh_frame_hdr)
    *(.comment)
    *(.note*)
  }
}
