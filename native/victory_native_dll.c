// The Windows side of victory_native: holds no code, only gives Wine a
// builtin library whose unix side it loads.
int __stdcall DllMain(void *module, unsigned reason, void *reserved)
{
    return 1;
}
