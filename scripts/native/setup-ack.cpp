#include <cassert>
#include <cstddef>
#include <cstdint>
#include <iomanip>
#include <iostream>
#include "../../vendor/ProtoInput/src/ProtoInput/ProtoInputHooks/pipeinclude/MuxSetupAckProtocol.h"

using namespace ProtoMuxSetupAck;
static_assert(offsetof(BeginV2, mapping) == 48, "BeginV2 config offset");
static_assert(offsetof(Install, hook) == 32, "install hook offset");
static_assert(offsetof(Install, reserved) == 40, "install reserved offset");
static_assert(offsetof(Reply, creation) == 16, "reply creation offset");
static_assert(offsetof(Reply, result) == 48, "reply result offset");

template<class T> void bytes(const char* name, const T& body, uint32_t id = 0)
{
    std::cout << name << '=' << std::hex << std::setfill('0');
    if (id)
    {
        const uint32_t header[]{id, sizeof(T)};
        for (const auto value : header)
            for (unsigned int shift = 0; shift < 32; shift += 8)
                std::cout << std::setw(2) << ((value >> shift) & 255);
    }
    const auto data = reinterpret_cast<const uint8_t*>(&body);
    for (size_t i = 0; i < sizeof(T); ++i) std::cout << std::setw(2) << static_cast<unsigned int>(data[i]);
    std::cout << '\n';
}

int main()
{
    // A legacy optimistic installed flag is deliberately true even on partial failure.
    for (int fail = 0; fail <= 4; ++fail)
    {
        bool installed[4]{};
        bool optimistic = false;
        uint32_t error = 0;
        int calls = 0;
        auto install = [&]()
        {
            optimistic = true;
            for (int index = 0; index < 4; ++index)
            {
                ++calls;
                installed[index] = fail != index + 1;
                if (!installed[index] && !error) error = 0xC0000022u;
            }
        };
        auto state = [&]()
        {
            bool any = false, all = true;
            for (bool value : installed) { any = any || value; all = all && value; }
            return Detours{any, all};
        };
        const auto outcome = CheckInstall(install, state, [&]() { return error; });
        assert(optimistic && calls == 4);
        assert(outcome.result == (fail ? NativeFailure : Applied));
        assert(outcome.native_status == (fail ? 0xC0000022u : 0));
        // A sticky failure must not be reset or permit another install.
        const auto second = CheckInstall(install, state, [&]() { return error; });
        assert(second.result == (fail ? Dirty : AlreadyApplied) && calls == 4);
    }
    // ACL failure after every detour succeeds is still a failed native setup.
    bool ready = false;
    uint32_t status = 0;
    const auto acl = CheckInstall([&]() { ready = true; status = 0x00000102u; },
        [&]() { return Detours{ready, ready}; }, [&]() { return status; });
    assert(acl.result == NativeFailure && acl.native_status == 0x00000102u);

    Install request{7, 8, 9, 1, 2, 1, 0};
    Ledger ledger;
    unsigned int executions = 0;
    auto apply = [&]() { ++executions; return Outcome{Applied, 0}; };
    assert(ledger.Run(request, 7, 8, 9, apply).result == Applied);
    assert(ledger.Run(request, 7, 8, 9, apply).result == Applied && executions == 1);
    auto altered = request;
    altered.hook = 3;
    assert(ledger.Run(altered, 7, 8, 9, apply).result == DuplicateConflict && executions == 1);
    for (unsigned int field = 0; field < 3; ++field)
    {
        auto stale = request;
        if (field == 0) ++stale.creation;
        if (field == 1) ++stale.generation;
        if (field == 2) ++stale.session;
        assert(ledger.Run(stale, 7, 8, 9, apply).result == StaleGeneration && executions == 1);
    }
    auto skipped = request;
    skipped.request = 3;
    assert(ledger.Run(skipped, 7, 8, 9, apply).result == Invalid && executions == 1);
    const BeginV2 begin{{7, 8, 9, 0x11, 0x22, 0x33}, 0x44, 0x55};
    const Reply reply{ReplyMagic, Version, sizeof(Reply), 2, 7, 8, 9, 1, Applied, 0, 2, 0};
    bytes("begin-checked", begin, 33);
    bytes("install-checked", request, 34);
    bytes("reply", reply);
}
