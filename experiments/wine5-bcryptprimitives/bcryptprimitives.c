#include <windows.h>
#include <bcrypt.h>
#include <limits.h>

/*
 * Wine 5 predates bcryptprimitives.dll, but already implements
 * BCryptGenRandom. Newer Windows Rust binaries import ProcessPrng directly.
 * This compatibility shim supplies that one documented function without
 * introducing another random-number generator.
 */
BOOL WINAPI ProcessPrng(PBYTE buffer, SIZE_T length)
{
    while (length != 0) {
        ULONG chunk = length > ULONG_MAX ? ULONG_MAX : (ULONG)length;
        NTSTATUS status = BCryptGenRandom(
            NULL,
            buffer,
            chunk,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG);

        if (status < 0) {
            return FALSE;
        }

        buffer += chunk;
        length -= chunk;
    }

    return TRUE;
}
