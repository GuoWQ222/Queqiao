// Queqiao virtual display driver: the swap-chain consumer. IddCx gives each active monitor a swap chain; the driver
// must keep taking its buffers and report each frame done (within 10 s, or the OS restarts the driver), otherwise the
// desktop on that monitor stops. Queqiao reads the picture with DXGI Desktop Duplication like on any screen, so here
// every buffer is released at once.
//
// Copyright (c) 2026 GuoWQ222. Part of Queqiao; see the LICENSE file of the Queqiao repository.
#include "driver.h"

#include <wrl/client.h>

using Microsoft::WRL::ComPtr;

namespace qqidd {

FrameConsumer* FrameConsumer::Start(IDDCX_SWAPCHAIN swapChain, LUID renderAdapter, HANDLE frameEvent,
                                    volatile LONG64* frameCounter, UINT32 monitorId) {
    auto* self = new (std::nothrow) FrameConsumer();
    if (!self) return nullptr;
    self->swapChain_ = swapChain;
    self->renderAdapter_ = renderAdapter;
    self->frameEvent_ = frameEvent;
    self->frameCounter_ = frameCounter;
    self->monitorId_ = monitorId;
    self->stopEvent_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (self->stopEvent_) self->thread_ = CreateThread(nullptr, 0, ThreadMain, self, 0, nullptr);
    if (!self->thread_) {
        if (self->stopEvent_) CloseHandle(self->stopEvent_);
        delete self;
        return nullptr;
    }
    return self;
}

void FrameConsumer::StopAndDelete(bool deleteSwapChain) {
    InterlockedExchange(&deleteSwapChain_, deleteSwapChain ? 1 : 0);
    SetEvent(stopEvent_);
    WaitForSingleObject(thread_, INFINITE);
    CloseHandle(thread_);
    CloseHandle(stopEvent_);
    delete this;
}

DWORD WINAPI FrameConsumer::ThreadMain(void* self) {
    static_cast<FrameConsumer*>(self)->Run();
    return 0;
}

void FrameConsumer::Run() {
    // The Multimedia Class Scheduler keeps this thread responsive when the CPU is busy.
    DWORD taskIndex = 0;
    HANDLE avTask = AvSetMmThreadCharacteristicsW(L"Distribution", &taskIndex);

    ComPtr<IDXGIFactory2> factory;
    ComPtr<IDXGIAdapter1> adapter;
    ComPtr<ID3D11Device> device;
    ComPtr<IDXGIDevice> dxgiDevice;
    HRESULT hr = CreateDXGIFactory2(0, IID_PPV_ARGS(&factory));
    ComPtr<IDXGIFactory4> factory4;
    if (SUCCEEDED(hr)) hr = factory.As(&factory4);
    if (SUCCEEDED(hr)) hr = factory4->EnumAdapterByLuid(renderAdapter_, IID_PPV_ARGS(&adapter));
    if (SUCCEEDED(hr))
        hr = D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr,
                               0, D3D11_SDK_VERSION, &device, nullptr, nullptr);
    if (SUCCEEDED(hr)) hr = device.As(&dxgiDevice);

    if (FAILED(hr)) {
        Log(L"monitor %u: no D3D device on render adapter %08x:%08x (0x%08x)", monitorId_, renderAdapter_.HighPart,
            renderAdapter_.LowPart, hr);
    } else {
        Consume(dxgiDevice.Get());
    }

    // Deleting the swap chain when the loop ends tells the OS to make a new one if the monitor is still active
    // (after a lost device, for example).
    if (InterlockedCompareExchange(&deleteSwapChain_, 0, 0)) WdfObjectDelete(reinterpret_cast<WDFOBJECT>(swapChain_));
    if (avTask) AvRevertMmThreadCharacteristics(avTask);
}

bool FrameConsumer::Consume(IDXGIDevice* device) {
    IDARG_IN_SWAPCHAINSETDEVICE setDevice = {};
    setDevice.pDevice = device;
    HRESULT hr = IddCxSwapChainSetDevice(swapChain_, &setDevice);
    if (FAILED(hr)) {
        Log(L"monitor %u: IddCxSwapChainSetDevice 0x%08x", monitorId_, hr);
        return false;
    }
    Log(L"monitor %u: consuming frames", monitorId_);
    for (;;) {
        if (WaitForSingleObject(stopEvent_, 0) == WAIT_OBJECT_0) return true;
        IDARG_OUT_RELEASEANDACQUIREBUFFER buffer = {};
        hr = IddCxSwapChainReleaseAndAcquireBuffer(swapChain_, &buffer);
        if (hr == E_PENDING) {
            HANDLE waits[2] = {frameEvent_, stopEvent_};
            DWORD r = WaitForMultipleObjects(2, waits, FALSE, 100);
            if (r == WAIT_OBJECT_0 + 1) return true;
            if (r != WAIT_OBJECT_0 && r != WAIT_TIMEOUT) {
                Log(L"monitor %u: wait failed %u", monitorId_, GetLastError());
                return false;
            }
            continue;
        }
        if (FAILED(hr)) {
            // The swap chain was abandoned (mode change, device lost): end; the OS assigns a new one if needed.
            Log(L"monitor %u: swap chain ended (0x%08x)", monitorId_, hr);
            return false;
        }
        if (buffer.MetaData.pSurface) buffer.MetaData.pSurface->Release();  // the acquire took a reference for us
        InterlockedIncrement64(frameCounter_);
        hr = IddCxSwapChainFinishedProcessingFrame(swapChain_);
        if (FAILED(hr)) {
            Log(L"monitor %u: IddCxSwapChainFinishedProcessingFrame 0x%08x", monitorId_, hr);
            return false;
        }
    }
}

}  // namespace qqidd
