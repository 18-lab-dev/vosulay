#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <dxgi.h>
#include <wrl/client.h>
#pragma comment(lib,"d3d11.lib")
#pragma comment(lib,"dxgi.lib")
using Microsoft::WRL::ComPtr;
static bool running=true;
static LRESULT CALLBACK proc(HWND h,UINT m,WPARAM w,LPARAM l){if(m==WM_CLOSE||m==WM_DESTROY){running=false;PostQuitMessage(0);return 0;}return DefWindowProcW(h,m,w,l);}
int WINAPI wWinMain(HINSTANCE instance,HINSTANCE,PWSTR,int){
 SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
 WNDCLASSW wc{};wc.lpfnWndProc=proc;wc.hInstance=instance;wc.lpszClassName=L"VosulayAutoresearchDx11";wc.hCursor=LoadCursor(nullptr,IDC_ARROW);if(!RegisterClassW(&wc))return 2;
 RECT r{0,0,640,480};AdjustWindowRectEx(&r,WS_OVERLAPPEDWINDOW,FALSE,0);
 HWND hwnd=CreateWindowExW(0,wc.lpszClassName,L"Vosulay autoresearch game",WS_OVERLAPPEDWINDOW,CW_USEDEFAULT,CW_USEDEFAULT,r.right-r.left,r.bottom-r.top,nullptr,nullptr,instance,nullptr);if(!hwnd)return 3;
 DXGI_SWAP_CHAIN_DESC sd{};sd.BufferDesc.Width=640;sd.BufferDesc.Height=480;sd.BufferDesc.Format=DXGI_FORMAT_B8G8R8A8_UNORM;sd.SampleDesc.Count=1;sd.BufferUsage=DXGI_USAGE_RENDER_TARGET_OUTPUT;sd.BufferCount=2;sd.OutputWindow=hwnd;sd.Windowed=TRUE;sd.SwapEffect=DXGI_SWAP_EFFECT_DISCARD;
 ComPtr<ID3D11Device> dev;ComPtr<ID3D11DeviceContext> ctx;ComPtr<IDXGISwapChain> swap;D3D_FEATURE_LEVEL level{};const D3D_FEATURE_LEVEL levels[]={D3D_FEATURE_LEVEL_11_1,D3D_FEATURE_LEVEL_11_0};
 HRESULT hr=D3D11CreateDeviceAndSwapChain(nullptr,D3D_DRIVER_TYPE_HARDWARE,nullptr,0,levels,2,D3D11_SDK_VERSION,&sd,&swap,&dev,&level,&ctx);if(FAILED(hr))return 4;
 ComPtr<ID3D11Texture2D> back;ComPtr<ID3D11RenderTargetView> rtv;if(FAILED(swap->GetBuffer(0,IID_PPV_ARGS(&back)))||FAILED(dev->CreateRenderTargetView(back.Get(),nullptr,&rtv)))return 5;
 // Give the late-injected window hook real messages even without user input.
 if(!SetTimer(hwnd,1,1000,nullptr))return 7;
 ShowWindow(hwnd,SW_SHOW);UpdateWindow(hwnd);
 while(running){MSG msg;while(PeekMessageW(&msg,nullptr,0,0,PM_REMOVE)){TranslateMessage(&msg);DispatchMessageW(&msg);}if(!running)break;const float bg[]={0.035f,0.12f,0.22f,1.0f};ctx->OMSetRenderTargets(1,rtv.GetAddressOf(),nullptr);ctx->ClearRenderTargetView(rtv.Get(),bg);hr=swap->Present(0,0);if(FAILED(hr))return 6;}
 return 0;
}
