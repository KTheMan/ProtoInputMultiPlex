// Compile against the actual pinned headers; no vendor source is modified.
// Prints canonical zero-padded native objects for comparison with the Rust codec.
#include <windows.h>
#include <cstddef>
#include <cstdio>
#include <cstring>
// These are declarations only; this executable must not export/link loader APIs.
#pragma push_macro("__declspec")
#undef __declspec
#define __declspec(...)
#include "../../vendor/ProtoInput/src/ProtoInput/ProtoInputLoader/include/protoloader.h"
#pragma pop_macro("__declspec")
#ifdef _MSC_VER
// The pinned header initializes unsigned device IDs with the -1 sentinel.
// Suppress its signed-conversion warning without weakening fixture diagnostics.
#pragma warning(push)
#pragma warning(disable: 4245)
#endif
#include "../../vendor/ProtoInput/src/ProtoInput/ProtoInputHooks/pipeinclude/pipeinclude.h"
#ifdef _MSC_VER
#pragma warning(pop)
#endif

using namespace ProtoPipe;
static_assert(sizeof(bool) == 1 && sizeof(unsigned int) == 4 && sizeof(int) == 4, "scalar ABI");
static_assert(sizeof(PipeMessageType) == 4 && sizeof(PipeMessageHeader) == 8, "header ABI");
static_assert(sizeof(PipeMessageSetupHook) == 8 && offsetof(PipeMessageSetupHook, install) == 4, "hook ABI");
static_assert(sizeof(PipeMessageSetupMessageFilter) == 8, "filter ABI");
static_assert(sizeof(PipeMessageSetupMessageBlock) == 8, "block ABI");
static_assert(offsetof(PipeMessageSetupMessageFilter, enable) == 4, "filter bool offset");
static_assert(offsetof(PipeMessageSetupMessageBlock, block) == 4, "block bool offset");
static_assert(sizeof(PipeMessageStartFocusMessageLoop) == 12, "focus ABI");
static_assert(offsetof(PipeMessageStartFocusMessageLoop, wm_activate) == 4
    && offsetof(PipeMessageStartFocusMessageLoop, wm_activateapp) == 5
    && offsetof(PipeMessageStartFocusMessageLoop, wm_ncactivate) == 6
    && offsetof(PipeMessageStartFocusMessageLoop, wm_setfocus) == 7
    && offsetof(PipeMessageStartFocusMessageLoop, wm_mouseactivate) == 8, "focus boolean ordering");
static_assert(sizeof(PipeMessageStopFocusMessageLoop) == 1, "empty body ABI");
static_assert(sizeof(PipeMessageSetupMessagesToSend) == 4
    && offsetof(PipeMessageSetupMessagesToSend, sendMouseWheelMessages) == 0
    && offsetof(PipeMessageSetupMessagesToSend, sendMouseButtonMessages) == 1
    && offsetof(PipeMessageSetupMessagesToSend, sendMouseMoveMessages) == 2
    && offsetof(PipeMessageSetupMessagesToSend, sendKeyboardPressMessages) == 3, "message boolean ordering");
static_assert(sizeof(PipeMesasgeAddSelectedMouseOrKeyboard) == 8
    && offsetof(PipeMesasgeAddSelectedMouseOrKeyboard, keyboard) == 4, "device ABI");
static_assert(sizeof(PipeMessageSetAllowFakeCursorOutOfBounds) == 2
    && offsetof(PipeMessageSetAllowFakeCursorOutOfBounds, extendBounds) == 1, "bounds ABI");
static_assert(offsetof(MuxInputPacket, device) == 0 && offsetof(MuxInputPacket, kind) == 4
    && offsetof(MuxInputPacket, flags) == 8 && offsetof(MuxInputPacket, x) == 12
    && offsetof(MuxInputPacket, y) == 16 && offsetof(MuxInputPacket, data) == 20, "input field ordering");

static void hex(const void* data, size_t bytes)
{
    const auto p = static_cast<const unsigned char*>(data);
    for (size_t i = 0; i < bytes; ++i) std::printf("%02x", p[i]);
}

template<class T, class Init> static void emit(PipeMessageType id, Init init)
{
    T value;
    std::memset(static_cast<void*>(&value), 0, sizeof(value));
    init(value);
    const PipeMessageHeader header{id, static_cast<unsigned int>(sizeof(value))};
    std::printf("%u=", static_cast<unsigned int>(id));
    hex(&header, sizeof(header));
    std::printf(":");
    hex(&value, sizeof(value));
    std::printf("\n");
}

int main()
{
    constexpr uint64_t token = 0x8877665544332211ULL;
    emit<PipeMessageSetupHook>(PipeMessageType::SetupHook, [](auto& v) { v.hookID = FocusHooksHookID; v.install = true; });
    emit<PipeMessageSetupMessageFilter>(PipeMessageType::SetupMessageFilter, [](auto& v) { v.filterID = KeyboardButtonFilterID; v.enable = false; });
    emit<PipeMessageSetupMessageBlock>(PipeMessageType::SetupMessageBlock, [](auto& v) { v.message = 0x2a3; v.block = true; });
    emit<PipeMesasgeUpdateMainWindowHandle>(PipeMessageType::UpdateMainWindowHandle, [token](auto& v) { v.hwnd = token; });
    emit<PipeMessageStartFocusMessageLoop>(PipeMessageType::StartFocusMessageLoop, [](auto& v) { v.milliseconds = 10; v.wm_activate = true; v.wm_ncactivate = true; v.wm_mouseactivate = true; });
    emit<PipeMessageSetupState>(PipeMessageType::SetupState, [](auto& v) { v.instanceNumber = 2; });
    emit<PipeMessageSetupMessagesToSend>(PipeMessageType::SetupMessagesToSend, [](auto& v) { v.sendMouseWheelMessages = true; v.sendMouseMoveMessages = true; });
    emit<PipeMessageSetDrawFakeCursor>(PipeMessageType::SetDrawFakeCursor, [](auto& v) { v.enable = false; });
    emit<PipeMessageSetExternalFreezeFakeInput>(PipeMessageType::SetExternalFreezeFakeInput, [](auto& v) { v.freezeEnabled = true; });
    emit<PipeMesasgeAddSelectedMouseOrKeyboard>(PipeMessageType::AddSelectedMouseOrKeyboard, [](auto& v) { v.mouse = 0xf1000002; v.keyboard = 0xffffffff; });
    emit<PipeMesasgeAddSelectedMouseOrKeyboard>(PipeMessageType::AddSelectedMouseOrKeyboard, [](auto& v) { v.mouse = 0xffffffff; v.keyboard = 0xf1000003; });
    emit<PipeMessageStopFocusMessageLoop>(PipeMessageType::StopFocusMessageLoop, [](auto&) {});
    emit<PipeMessageSetAllowFakeCursorOutOfBounds>(PipeMessageType::SetAllowFakeCursorOutOfBounds, [](auto& v) { v.allowOutOfBounds = false; v.extendBounds = true; });
    emit<MuxInputPacket>(PipeMessageType::MuxInput, [](auto& v) { v.device = 0xf1000002; v.x = -2; v.y = 32767; });
    emit<MuxInputPacket>(PipeMessageType::MuxInput, [](auto& v) { v.device = 0xf1000003; v.kind = 2; v.data = 0xd83d; });
    emit<MuxQueueFencePacket>(PipeMessageType::MuxQueueFence, [token](auto& v) { v.hwnd = 1; v.event = 2; v.cookie = token; });
    emit<MuxCursorObserverStartPacket>(PipeMessageType::MuxCursorObserverStart, [token](auto& v) { v.mapping = 1; v.mutex = 2; v.attachment = token; });
    emit<MuxCursorObserverAssociatePacket>(PipeMessageType::MuxCursorObserverAssociate, [token](auto& v) { v.hwnd = 1; v.association = token; });
    emit<MuxCursorObserverStopPacket>(PipeMessageType::MuxCursorObserverStop, [token](auto& v) { v.attachment = token; });
    emit<ProtoMuxLifecycle::BeginPacket>(PipeMessageType::MuxLifecycleBegin, [token](auto& v) { v.creation = token; v.generation = 2; v.session = 3; v.remote_commit_mapping = 4; v.remote_reply_mapping = 5; v.remote_event = 6; });
    emit<ProtoMuxLifecycle::RetirePacket>(PipeMessageType::MuxLifecyclePrepareRetire, [token](auto& v) { v.generation = token; v.session = 2; v.remote_reply_mapping = 3; v.remote_event = 4; });
    emit<MuxKeyboardStateCommandPacket>(PipeMessageType::MuxKeyboardStateCommand, [token](auto& v) { v.epoch = token; v.mapping = 1; v.event = 2; v.action = 2; });
}
