#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <wrl/client.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <wchar.h>
#include <chrono>
#include <fstream>
#include <string>

using Microsoft::WRL::ComPtr;
using Clock = std::chrono::steady_clock;
constexpr UINT marker_width = 48, marker_height = 8;

static bool decode(const uint8_t* pixels, UINT pitch, uint32_t& id) {
    uint8_t bytes[6]{};
    for (UINT x = 0; x < marker_width; ++x) {
        const uint8_t* first = pixels + x * 4;
        if (first[0] != first[1] || first[1] != first[2] || (first[0] != 0 && first[0] != 255)) return false;
        for (UINT y = 1; y < marker_height; ++y) {
            const uint8_t* row = pixels + y * pitch + x * 4;
            if (row[0] != first[0] || row[1] != first[1] || row[2] != first[2]) return false;
        }
        bytes[x / 8] = static_cast<uint8_t>((bytes[x / 8] << 1) | (first[0] != 0));
    }
    if (bytes[0] != 0xa7) return false;
    uint8_t crc = 0;
    for (int i = 1; i <= 4; ++i) {
        crc ^= bytes[i];
        for (int bit = 0; bit < 8; ++bit)
            crc = static_cast<uint8_t>((crc << 1) ^ ((crc & 0x80) ? 0x07 : 0));
    }
    if (crc != bytes[5]) return false;
    id = static_cast<uint32_t>(bytes[1]) | (static_cast<uint32_t>(bytes[2]) << 8) |
         (static_cast<uint32_t>(bytes[3]) << 16) | (static_cast<uint32_t>(bytes[4]) << 24);
    return true;
}

static bool save_bmp(const wchar_t* csv, const uint8_t* pixels, UINT pitch) {
    std::wstring path(csv);
    path += L".marker.bmp";
    FILE* file = nullptr;
    if (_wfopen_s(&file, path.c_str(), L"wb") || !file) return false;
    BITMAPFILEHEADER header{};
    header.bfType = 0x4d42;
    header.bfOffBits = sizeof(BITMAPFILEHEADER) + sizeof(BITMAPINFOHEADER);
    header.bfSize = header.bfOffBits + marker_width * marker_height * 3;
    BITMAPINFOHEADER info{};
    info.biSize = sizeof(info);
    info.biWidth = marker_width;
    info.biHeight = marker_height;
    info.biPlanes = 1;
    info.biBitCount = 24;
    info.biCompression = BI_RGB;
    info.biSizeImage = marker_width * marker_height * 3;
    bool ok = fwrite(&header, sizeof(header), 1, file) == 1 && fwrite(&info, sizeof(info), 1, file) == 1;
    uint8_t row[marker_width * 3];
    for (int y = static_cast<int>(marker_height) - 1; y >= 0 && ok; --y) {
        for (UINT x = 0; x < marker_width; ++x) {
            const uint8_t* pixel = pixels + static_cast<UINT>(y) * pitch + x * 4;
            for (UINT channel = 0; channel < 3; ++channel) row[x * 3 + channel] = pixel[channel];
        }
        ok = fwrite(row, sizeof(row), 1, file) == 1;
    }
    return fclose(file) == 0 && ok;
}

struct AcquiredFrame {
    IDXGIOutputDuplication* duplication;
    ~AcquiredFrame() { duplication->ReleaseFrame(); }
};

int wmain(int argc, wchar_t** argv) {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    if (argc != 5 || wcscmp(argv[1], L"observe") != 0) {
        fwprintf(stderr, L"usage: windows_native observe HWND seconds csv\n");
        return 2;
    }
    wchar_t* end = nullptr;
    HWND window = reinterpret_cast<HWND>(static_cast<uintptr_t>(_wcstoui64(argv[2], &end, 10)));
    if (!end || *end || !IsWindow(window)) return 2;
    const double seconds = wcstod(argv[3], &end);
    if (!end || *end || !(seconds > 0 && seconds <= 3600)) return 2;
    RECT client{};
    if (!GetClientRect(window, &client) || client.right != 640 || client.bottom != 480 || IsIconic(window)) return 3;
    POINT origin{68, 68};
    if (!ClientToScreen(window, &origin)) return 3;

    ComPtr<IDXGIFactory1> factory;
    if (FAILED(CreateDXGIFactory1(IID_PPV_ARGS(&factory)))) return 4;
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    ComPtr<IDXGIOutputDuplication> duplication;
    RECT output_rect{};
    for (UINT ai = 0; !duplication; ++ai) {
        ComPtr<IDXGIAdapter1> adapter;
        const HRESULT enumerated = factory->EnumAdapters1(ai, &adapter);
        if (enumerated == DXGI_ERROR_NOT_FOUND) break;
        if (FAILED(enumerated)) return 4;
        DXGI_ADAPTER_DESC1 adapter_desc{};
        if (FAILED(adapter->GetDesc1(&adapter_desc))) return 4;
        if (adapter_desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) continue;
        for (UINT oi = 0; !duplication; ++oi) {
            ComPtr<IDXGIOutput> output;
            const HRESULT result = adapter->EnumOutputs(oi, &output);
            if (result == DXGI_ERROR_NOT_FOUND) break;
            if (FAILED(result)) return 4;
            DXGI_OUTPUT_DESC desc{};
            if (FAILED(output->GetDesc(&desc))) return 4;
            if (!desc.AttachedToDesktop || origin.x < desc.DesktopCoordinates.left || origin.x >= desc.DesktopCoordinates.right ||
                origin.y < desc.DesktopCoordinates.top || origin.y >= desc.DesktopCoordinates.bottom) continue;
            D3D_FEATURE_LEVEL level{};
            if (FAILED(D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, nullptr, 0,
                                        D3D11_SDK_VERSION, &device, &level, &context)) || level < D3D_FEATURE_LEVEL_11_0) return 5;
            ComPtr<IDXGIOutput1> output1;
            if (FAILED(output.As(&output1)) || FAILED(output1->DuplicateOutput(device.Get(), &duplication))) return 6;
            output_rect = desc.DesktopCoordinates;
        }
    }
    if (!duplication) return 7;
    DXGI_OUTDUPL_DESC desktop{};
    duplication->GetDesc(&desktop);
    if (desktop.ModeDesc.Format != DXGI_FORMAT_B8G8R8A8_UNORM || desktop.Rotation != DXGI_MODE_ROTATION_IDENTITY) return 7;
    const LONG x = origin.x - output_rect.left, y = origin.y - output_rect.top;
    if (x < 0 || y < 0 || static_cast<UINT>(x) + marker_width > desktop.ModeDesc.Width ||
        static_cast<UINT>(y) + marker_height > desktop.ModeDesc.Height) return 7;
    D3D11_TEXTURE2D_DESC texture{};
    texture.Width = marker_width;
    texture.Height = marker_height;
    texture.MipLevels = texture.ArraySize = 1;
    texture.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    texture.SampleDesc.Count = 1;
    texture.Usage = D3D11_USAGE_STAGING;
    texture.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ComPtr<ID3D11Texture2D> staging;
    if (FAILED(device->CreateTexture2D(&texture, nullptr, &staging))) return 8;
    const D3D11_BOX box{static_cast<UINT>(x), static_cast<UINT>(y), 0,
                        static_cast<UINT>(x) + marker_width, static_cast<UINT>(y) + marker_height, 1};
    std::wofstream csv(argv[4], std::ios::trunc);
    if (!csv) return 9;
    csv << L"elapsed_ms,frame_id,valid\n";
    const auto started = Clock::now();
    auto last_sample = started;
    uint64_t frames = 0, valid = 0, changes = 0, skipped = 0;
    uint32_t previous = 0;
    bool have_previous = false, saved = false;
    double maximum_gap_ms = 0;
    while (std::chrono::duration<double>(Clock::now() - started).count() < seconds) {
        ComPtr<IDXGIResource> resource;
        DXGI_OUTDUPL_FRAME_INFO frame_info{};
        const HRESULT acquired = duplication->AcquireNextFrame(100, &frame_info, &resource);
        if (acquired == DXGI_ERROR_WAIT_TIMEOUT) continue;
        if (FAILED(acquired)) return 10;
        AcquiredFrame lease{duplication.Get()};
        ComPtr<ID3D11Texture2D> image;
        if (FAILED(resource.As(&image))) return 10;
        context->CopySubresourceRegion(staging.Get(), 0, 0, 0, 0, image.Get(), 0, &box);
        D3D11_MAPPED_SUBRESOURCE mapping{};
        if (FAILED(context->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapping))) return 10;
        uint32_t id = 0;
        const bool decoded = decode(static_cast<const uint8_t*>(mapping.pData), mapping.RowPitch, id);
        const auto now = Clock::now();
        const double gap = std::chrono::duration<double, std::milli>(now - last_sample).count();
        if (gap > maximum_gap_ms) maximum_gap_ms = gap;
        last_sample = now;
        if (decoded && !saved) saved = save_bmp(argv[4], static_cast<const uint8_t*>(mapping.pData), mapping.RowPitch);
        context->Unmap(staging.Get(), 0);
        ++frames;
        if (decoded) {
            if (have_previous) {
                const uint32_t delta = id - previous;
                if (delta >= 0x80000000U) return 13;
                if (delta) { ++changes; skipped += static_cast<uint64_t>(delta) - 1; }
            }
            previous = id;
            have_previous = true;
            ++valid;
        }
        csv << std::chrono::duration<double, std::milli>(now - started).count() << L"," << id << L"," << (decoded ? 1 : 0) << L"\n";
    }
    csv.flush();
    if (!csv) return 9;
    const double elapsed = std::chrono::duration<double>(Clock::now() - started).count();
    wprintf(L"observer_frames=%llu valid=%llu transitions=%llu skipped_ids=%llu elapsed=%.6f observer_hz=%.3f visible_updates_per_second=%.3f max_gap_ms=%.3f display=%ux%u capture=DXGI_DWM_composed_lower_bound\n",
            static_cast<unsigned long long>(frames), static_cast<unsigned long long>(valid),
            static_cast<unsigned long long>(changes), static_cast<unsigned long long>(skipped), elapsed,
            static_cast<double>(frames) / elapsed, static_cast<double>(changes) / elapsed, maximum_gap_ms,
            desktop.ModeDesc.Width, desktop.ModeDesc.Height);
    return valid > 1 && changes > 0 && saved && frames - valid <= frames / 20 ? 0 : 11;
}
