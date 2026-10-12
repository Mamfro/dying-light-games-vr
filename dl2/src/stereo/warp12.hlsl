// Two eye images from one rendered image and its depth (warp12.rs). Each source pixel moves sideways by
// its disparity (Splat: the nearer pixel wins), then every eye pixel takes its winner's colour, and gaps
// the centre camera could not see continue the farther (background) neighbour (Resolve).
cbuffer Warp:register(b0){uint W,H,DW,DH;float Scale,Direction;uint MaxShift,HasUi;float4 HudRect;float MarkerCount,AimX,AimY,CrossRadius,HudFromFrame,GridOffsetX,GridScaleX,GridScaleY,GridOffsetY,Pieces,MarkerBase,Pad;float4 Piece[5];};
Texture2D<float4> Color:register(t0);Texture2D<float> Depth:register(t1);Texture2D<float4> Ui:register(t2);Texture2D<float4> Fallback:register(t3);
// The HUD's world markers of this frame: each one's box in the UI layer (x, y, width, height) and its disparity in
// pixels (half per eye) at its target's distance; MarkerCount of them from MarkerBase.
struct Marker{float4 rect;float shift;float3 pad;};
StructuredBuffer<Marker> Markers:register(t4);
bool InRect(float2 s,float4 r){return s.x>=r.x && s.y>=r.y && s.x<r.x+r.z && s.y<r.y+r.w;}
bool InMarker(float2 s){for(int i=0;i<int(MarkerCount);++i)if(InRect(s,Markers[int(MarkerBase)+i].rect))return true;return false;}
// Whether a UI layer pixel lies in one of the dynamic HUD's pieces (x, y, width, height): shown on the hands, not here.
bool InPiece(float2 s){[unroll] for(int i=0;i<5;++i){if(float(i)>=Pieces)break;float4 r=Piece[i];if(s.x>=r.x && s.y>=r.y && s.x<r.x+r.z && s.y<r.y+r.w)return true;}return false;}
// The most opaque UI pixel within one pixel: soft HUD edges count as HUD.
float UiCover(int2 p){float m=0;[unroll] for(int dy=-1;dy<=1;++dy)[unroll] for(int dx=-1;dx<=1;++dx){
    int2 q=clamp(p+int2(dx,dy),int2(0,0),int2(int(W)-1,int(H)-1));m=max(m,Ui.Load(int3(q,0)).a);}return m;}
// The UI layer laid over pixel id: the whole HUD in HudRect (this eye's place for it: hud.rs, monaka_core), the
// crosshair at the game's aim point, the pieces on the hands left out.
void LayHud(inout float4 c,uint2 id){
    float2 p=float2(id)+0.5;float2 centre=float2(W,H)*0.5;float2 layer=float2(W,H);float4 u;
    float2 s=LayerPixel(p,HudRect,layer);
    if(InLayer(s) && !(length(s-centre)<CrossRadius || InPiece(s) || InMarker(s))){
        u=Ui.Load(int3(s,0));   // HUD colours: the fallback image (2), the frame's own pixels (1), or the UI layer (0)
        c.rgb=HudFromFrame>1.5?lerp(c.rgb,Fallback.Load(int3(s,0)).rgb,u.a):HudFromFrame>0.5?lerp(c.rgb,Color.Load(int3(s,0)).rgb,u.a):OverPremultiplied(c.rgb,u);}
    // The world markers where the game drew them (no compaction), each moved by its own disparity: at its target's depth.
    for(int i=0;i<int(MarkerCount);++i){Marker m=Markers[int(MarkerBase)+i];float2 s=p-float2(Direction*m.shift,0);
        if(InRect(s,m.rect)){u=Ui.Load(int3(s,0));c.rgb=OverPremultiplied(c.rgb,u);}}
    // The crosshair: the layer's centre drawn at the aim point, at the layer's scale.
    s=centre+(p-(HudRect.xy+HudRect.zw*0.5)-float2(AimX,AimY))*layer/HudRect.zw;
    if(length(s-centre)<CrossRadius && !InPiece(s)){u=Ui.Load(int3(s,0));c.rgb=OverPremultiplied(c.rgb,u);}}
RWTexture2D<uint> Keys:register(u0);RWTexture2D<unorm float4> Out:register(u1);
[numthreads(8,8,1)] void Clear(uint3 id:SV_DispatchThreadID){if(id.x<W && id.y<H)Keys[id.xy]=0;}
[numthreads(8,8,1)] void Splat(uint3 id:SV_DispatchThreadID){
    if(id.x>=W || id.y>=H)return;
    // Solid HUD (text, icons) hides the world: leave a gap to fill. Translucent HUD (backing panels) is un-blended
    // in Resolve; the UI layer is premultiplied (colour is 0 wherever alpha is 0).
    // With a fallback image (HudFromFrame 2) every HUD-touched pixel is left for it, translucent HUD included.
    // HasUi 2: the HUD was kept out of the frame (its draws went onto the UI layer only), so nothing to leave.
    if(HasUi==1 && UiCover(int2(id.xy))>=(HudFromFrame>1.5?0.004:0.25))return;
    float d=saturate(Depth.Load(int3(id.x*DW/W,id.y*DH/H,0)));
    int tx=int(round(float(id.x)+Direction*min(Scale*d,float(MaxShift))));
    if(tx<0 || tx>=int(W))return;
    InterlockedMax(Keys[uint2(tx,id.y)],((uint(d*1048574.0)+1)<<12)|id.x);}
[numthreads(8,8,1)] void Resolve(uint3 id:SV_DispatchThreadID){
    if(id.x>=W || id.y>=H)return;
    float2 gridScale=float2(GridScaleX>0?GridScaleX:1,GridScaleY>0?GridScaleY:1);
    int2 q=int2(floor((float2(id.xy)+0.5-float2(GridOffsetX,GridOffsetY))/gridScale));   // this pixel in the frame's grid
    bool outside=q.x<0 || q.y<0 || q.x>=int(W) || q.y>=int(H);q=clamp(q,int2(0,0),int2(int(W)-1,int(H)-1));
    uint key=outside?0:Keys[q];int sx=int(key&4095);bool fallen=!key && HudFromFrame>1.5;
    if(fallen){}
    else if(!key){uint best=0;int offset=0;
        [loop] for(int r=1;r<=256 && !best;++r){int xl=q.x-r,xr=q.x+r;
            uint kl=xl>=0?Keys[uint2(xl,q.y)]:0,kr=xr<int(W)?Keys[uint2(xr,q.y)]:0;
            if(kl && (!kr || (kl>>12)<=(kr>>12))){best=kl;offset=r;}else if(kr){best=kr;offset=-r;}}
        if(!best){Out[id.xy]=float4(0,0,0,1);return;}
        sx=clamp(int(best&4095)+offset,0,int(W)-1);
        // Continuing the background must not walk back onto solid HUD in the frame; stretch the neighbour instead.
        if(HasUi==1 && UiCover(int2(sx,q.y))>=0.25)sx=int(best&4095);}
    float4 c=fallen?Fallback.Load(int3(id.xy,0)):Color.Load(int3(sx,q.y,0));
    if(HasUi){float4 u=Ui.Load(int3(sx,q.y,0));if(HasUi==1 && !fallen && u.a>0.004 && u.a<0.25)c.rgb=Unblend(c.rgb,u);   // the world under a translucent HUD
        LayHud(c,id.xy);}
    Out[id.xy]=c;}
// A rendered eye (alternate-eye, same-frame stereo) with the UI layer laid over it: no warp.
[numthreads(8,8,1)] void Overlay(uint3 id:SV_DispatchThreadID){
    if(id.x>=W || id.y>=H)return;
    float4 c=Color.Load(int3(id.xy,0));
    if(HasUi)LayHud(c,id.xy);
    Out[id.xy]=c;}
