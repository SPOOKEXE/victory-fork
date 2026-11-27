#define RS "RootConstants(num32BitConstants=6, b0), SRV(t0), UAV(u0), UAV(u1)"
cbuffer Patch : register(b0) {
    uint count;
    uint first;
    uint unused0;
    uint unused1;
    uint unused2;
    uint unused3;
};
ByteAddressBuffer words : register(t0);
RWByteAddressBuffer copy : register(u0);
RWByteAddressBuffer unused : register(u1);
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= count) return;
    uint3 w = words.Load3((first + id.x) * 12u);
    copy.Store(w.x, (copy.Load(w.x) & ~w.y) | (w.z & w.y));
}
