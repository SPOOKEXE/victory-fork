#define RS "RootConstants(num32BitConstants=1, b0), SRV(t0), SRV(t1), UAV(u0), UAV(u1)"
cbuffer Check : register(b0) {
    uint words;
};
ByteAddressBuffer window_record : register(t0);
ByteAddressBuffer fresh_record : register(t1);
RWByteAddressBuffer results : register(u0);
RWByteAddressBuffer fresh_budget : register(u1);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() {
    bool same = true;
    for (uint i = 0; i < words; i++) {
        if (window_record.Load(4 * i) != fresh_record.Load(4 * i)) {
            same = false;
        }
    }
    uint was;
    results.InterlockedAdd(4, 1, was);
    if (!same) {
        uint before;
        results.InterlockedAdd(0, 1, before);
        if (before == 0) {
            for (uint k = 0; k < min(words, 8u); k++) {
                results.Store(8 + 4 * k, window_record.Load(4 * k));
                results.Store(40 + 4 * k, fresh_record.Load(4 * k));
            }
        }
    }
    fresh_budget.Store3(0, uint3(0, 0, 0));
}
