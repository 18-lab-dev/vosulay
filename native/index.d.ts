export interface AttachOptions {
  vosuUrl?: string;
  attachToolPath?: string;
  profilerLibraryPath?: string;
  agentLibraryPath?: string;
  timeout?: number;
}
export interface SurfaceDescriptor { targetId: number; width: number; height: number; stride: number; offset: number; size: number; format?: number; fd?: number }
export interface SharedTexturePlane { fd: number; stride: number; offset: number; size: number }
export interface SharedTextureDescriptor { targetId: number; sequence: number; width: number; height: number; fourcc: "AR24" | "AB24"; modifier: string; planes: SharedTexturePlane[] }
export interface OverlaySurface { updateSharedTexture(descriptor: SurfaceDescriptor): Promise<void>; updateDescriptor(descriptor: SurfaceDescriptor): Promise<void>; updateDmabuf(descriptor: SharedTextureDescriptor): Promise<void>; clear(targetId: number): Promise<void> }
export interface OverlayHandle { readonly pid: number; socketName(): string; cookie(): string; destroy(): Promise<void>; setVisible(visible: boolean): Promise<void>; setEditing(editing: boolean): Promise<void>; captureInput(capture: boolean): Promise<void>; createSurface(): OverlaySurface; pollEvent(): string; on(event: "disconnect" | "input" | "end" | "consumed" | "frame" | "target", listener: (value: string) => void): void }
export function attach(pid: number, options?: AttachOptions): Promise<OverlayHandle>
