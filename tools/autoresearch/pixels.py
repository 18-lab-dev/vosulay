"""Read-only X11 window capture and autoresearch marker decoding."""
import ctypes as C
from PIL import Image

WINDOW_TITLE = 'Vosulay autoresearch game'
MARKER_X, MARKER_Y, MARKER_WIDTH, MARKER_HEIGHT = 4, 4, 48, 8
MARKER_MAGIC = 0xA7

class XImage(C.Structure):
    _fields_ = [('width', C.c_int), ('height', C.c_int), ('xoffset', C.c_int), ('format', C.c_int), ('data', C.c_void_p), ('byte_order', C.c_int), ('bitmap_unit', C.c_int), ('bitmap_bit_order', C.c_int), ('bitmap_pad', C.c_int), ('depth', C.c_int), ('bytes_per_line', C.c_int), ('bits_per_pixel', C.c_int), ('red_mask', C.c_ulong), ('green_mask', C.c_ulong), ('blue_mask', C.c_ulong)]

class XWindowAttributes(C.Structure):
    _fields_ = [('x',C.c_int),('y',C.c_int),('width',C.c_int),('height',C.c_int),('border_width',C.c_int),('depth',C.c_int),('visual',C.c_void_p),('root',C.c_ulong),('class_',C.c_int),('bit_gravity',C.c_int),('win_gravity',C.c_int),('backing_store',C.c_int),('backing_planes',C.c_ulong),('backing_pixel',C.c_ulong),('save_under',C.c_int),('colormap',C.c_ulong),('map_installed',C.c_int),('map_state',C.c_int),('all_event_masks',C.c_long),('your_event_mask',C.c_long),('do_not_propagate_mask',C.c_long),('override_redirect',C.c_int),('screen',C.c_void_p)]


def crc8(frame_id):
    value = 0
    for byte in int(frame_id).to_bytes(4, 'little'):
        value ^= byte
        for _ in range(8):
            value = ((value << 1) ^ 0x07) & 0xff if value & 0x80 else (value << 1) & 0xff
    return value


def decode(rgb_image):
    """Decode ID from 48x8 capture; return None for blanks, torn/gray pixels, or bad CRC."""
    image = rgb_image.convert('RGB')
    if image.size != (MARKER_WIDTH, MARKER_HEIGHT): return None
    rows=[]
    for y in range(MARKER_HEIGHT):
        bits=[]
        for x in range(MARKER_WIDTH):
            r,g,b=image.getpixel((x,y))
            if r != g or g != b or r not in (0,255): return None
            bits.append(r >> 7)
        rows.append(tuple(bits))
    if any(row != rows[0] for row in rows[1:]): return None
    bits=rows[0]
    raw=bytes(sum(bits[i+j] << (7-j) for j in range(8)) for i in range(0,48,8))
    if raw[0] != MARKER_MAGIC: return None
    frame_id=int.from_bytes(raw[1:5],'little')
    return frame_id if raw[5] == crc8(frame_id) else None


class X11:
    """Minimal read-only X11 capture. No focus, map, or input operations."""
    def __init__(self, display_name=None):
        self.x=C.CDLL('libX11.so.6')
        declarations=[('XOpenDisplay',C.c_void_p,[C.c_char_p]),('XCloseDisplay',C.c_int,[C.c_void_p]),('XDefaultRootWindow',C.c_ulong,[C.c_void_p]),
          ('XQueryTree',C.c_int,[C.c_void_p,C.c_ulong,C.POINTER(C.c_ulong),C.POINTER(C.c_ulong),C.POINTER(C.POINTER(C.c_ulong)),C.POINTER(C.c_uint)]),
          ('XFree',C.c_int,[C.c_void_p]),('XFetchName',C.c_int,[C.c_void_p,C.c_ulong,C.POINTER(C.c_void_p)]),('XInternAtom',C.c_ulong,[C.c_void_p,C.c_char_p,C.c_int]),
          ('XGetWindowProperty',C.c_int,[C.c_void_p,C.c_ulong,C.c_ulong,C.c_long,C.c_long,C.c_int,C.c_ulong,C.POINTER(C.c_ulong),C.POINTER(C.c_int),C.POINTER(C.c_ulong),C.POINTER(C.c_ulong),C.POINTER(C.c_void_p)]),
          ('XGetWindowAttributes',C.c_int,[C.c_void_p,C.c_ulong,C.POINTER(XWindowAttributes)]),('XGetImage',C.POINTER(XImage),[C.c_void_p,C.c_ulong,C.c_int,C.c_int,C.c_uint,C.c_uint,C.c_ulong,C.c_int]),('XDestroyImage',C.c_int,[C.POINTER(XImage)])]
        for name,restype,args in declarations:
            fn=getattr(self.x,name);fn.restype=restype;fn.argtypes=args
        self.display=self.x.XOpenDisplay(display_name.encode() if isinstance(display_name,str) else display_name)
        if not self.display: raise RuntimeError('cannot open X display')
        self.root=self.x.XDefaultRootWindow(self.display)

    def close(self):
        if self.display: self.x.XCloseDisplay(self.display);self.display=None
    def __enter__(self): return self
    def __exit__(self,*_): self.close()

    def _title(self,window):
        value=C.c_void_p();title=''
        if self.x.XFetchName(self.display,window,C.byref(value)) and value:
            title=C.string_at(value).decode(errors='replace');self.x.XFree(value)
        if title:return title
        atom=self.x.XInternAtom(self.display,b'_NET_WM_NAME',1);actual=C.c_ulong();fmt=C.c_int();count=C.c_ulong();remain=C.c_ulong();data=C.c_void_p()
        if atom and self.x.XGetWindowProperty(self.display,window,atom,0,1024,0,0,C.byref(actual),C.byref(fmt),C.byref(count),C.byref(remain),C.byref(data))==0 and data:
            try:
                if fmt.value==8:title=C.string_at(data,count.value).decode(errors='replace')
            finally:self.x.XFree(data)
        return title

    def _pid(self,window):
        atom=self.x.XInternAtom(self.display,b'_NET_WM_PID',1);actual=C.c_ulong();fmt=C.c_int();count=C.c_ulong();remain=C.c_ulong();data=C.c_void_p()
        if not atom or self.x.XGetWindowProperty(self.display,window,atom,0,1,0,0,C.byref(actual),C.byref(fmt),C.byref(count),C.byref(remain),C.byref(data))!=0 or not data:return None
        try:return C.cast(data,C.POINTER(C.c_ulong))[0] if fmt.value==32 and count.value else None
        finally:self.x.XFree(data)

    def _children(self,window):
        root=C.c_ulong();parent=C.c_ulong();children=C.POINTER(C.c_ulong)();count=C.c_uint()
        if not self.x.XQueryTree(self.display,window,C.byref(root),C.byref(parent),C.byref(children),C.byref(count)):return []
        try:return [children[i] for i in range(count.value)]
        finally:
            if children:self.x.XFree(children)

    def find_window(self,pid,title=WINDOW_TITLE):
        wanted=int(pid);stack=list(self._children(self.root));seen=set()
        while stack:
            window=stack.pop()
            if window in seen:continue
            seen.add(window);stack.extend(self._children(window))
            attrs=XWindowAttributes()
            if self.x.XGetWindowAttributes(self.display,window,C.byref(attrs)) and attrs.map_state==2 and self._pid(window)==wanted:
                if title is None or self._title(window)==title:return window
        return None

    def _capture(self,window,x,y,width,height):
        ptr=self.x.XGetImage(self.display,window,x,y,width,height,C.c_ulong(-1).value,2)
        if not ptr:raise RuntimeError('XGetImage failed')
        try:
            info=ptr.contents
            if info.format!=2 or info.bits_per_pixel!=32 or info.byte_order!=0 or info.bytes_per_line<info.width*4 or (info.red_mask,info.green_mask,info.blue_mask)!=(0xff0000,0xff00,0xff):raise RuntimeError('unsupported XImage format')
            data=C.string_at(info.data,info.bytes_per_line*info.height)
            return Image.frombytes('RGB',(info.width,info.height),data,'raw','BGRX',info.bytes_per_line)
        finally:self.x.XDestroyImage(ptr)

    def screenshot(self,window):
        attrs=XWindowAttributes()
        if not self.x.XGetWindowAttributes(self.display,window,C.byref(attrs)) or attrs.width<=0 or attrs.height<=0:raise RuntimeError('window unavailable')
        return self._capture(window,0,0,attrs.width,attrs.height)

    def marker(self,window,origin=(MARKER_X,MARKER_Y)):
        return self._capture(window,int(origin[0]),int(origin[1]),MARKER_WIDTH,MARKER_HEIGHT)

    def sample(self,pid,title=WINDOW_TITLE,origin=(MARKER_X,MARKER_Y)):
        window=self.find_window(pid,title)
        if window is None:raise LookupError(f'no mapped X11 window for PID {pid} and title {title!r}')
        return window,self.marker(window,origin)

    def screenshot_for(self,pid,title=WINDOW_TITLE):
        window=self.find_window(pid,title)
        if window is None:raise LookupError(f'no mapped X11 window for PID {pid} and title {title!r}')
        return self.screenshot(window)
