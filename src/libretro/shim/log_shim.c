/*
 * Variadic bridge for the libretro log callback.
 *
 * libretro hands cores a `void (*)(enum retro_log_level, const char *fmt, ...)`.
 * Stable Rust cannot define a variadic function — `c_variadic` is still
 * unstable as of 1.97 — and a non-variadic stand-in receives the format string
 * with none of its arguments substituted. That is not a cosmetic loss: Gambatte
 * logs every one of its messages as `log_cb(level, "[Gambatte] %s\n", text)`,
 * so the entire content lives in the varargs. Cores report missing BIOS files
 * the same way.
 *
 * So the formatting happens here, in nineteen lines of C, and the result goes
 * back to Rust as a plain string.
 */
#include <stdarg.h>
#include <stdio.h>

/* Implemented in Rust. */
void rustromm_core_log_line(unsigned level, const char *text);

void rustromm_log_shim(unsigned level, const char *fmt, ...)
{
    char buf[4096];
    va_list ap;
    va_start(ap, fmt);
    /* Truncation is fine and cannot overflow: vsnprintf always terminates. */
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    rustromm_core_log_line(level, buf);
}
