// The HUD layer laid over an eye frame (hudover.rs): the whole layer in HudRect (this eye's place for it, from
// monaka_core's hud placement), the crosshair at the game's aim point, the pieces on the hands left out.
cbuffer Over:register(b0){uint W,H;float CrossRadius,Pieces;float4 HudRect;float AimX,AimY,Pad0,Pad1;float4 Piece[5];};
Texture2D<float4> Frame:register(t0);Texture2D<float4> Ui:register(t1);
RWTexture2D<unorm float4> Out:register(u0);
// Whether a UI layer pixel lies in one of the dynamic HUD's pieces (x, y, width, height): shown on the hands, not here.
bool InPiece(float2 s){[unroll] for(int i=0;i<5;++i){if(float(i)>=Pieces)break;float4 r=Piece[i];if(s.x>=r.x && s.y>=r.y && s.x<r.x+r.z && s.y<r.y+r.w)return true;}return false;}
// A translucent black the game lays over the whole screen (it dims the world behind a wheel): its opacity, the
// least of the layer's four corners (0 unless all four are black; a wheel's own panel can darken one side more).
float Dim(){float lo=1;[unroll] for(int i=0;i<4;++i){int2 q=int2((i&1)?int(W)-3:2,(i&2)?int(H)-3:2);float4 u=Ui.Load(int3(q,0));
    if(dot(u.rgb,float3(1,1,1))>0.03)return 0;lo=min(lo,u.a);}return lo<0.98?lo:0;}
// A layer pixel with the dim taken out (laid over the whole eye instead): what was drawn over it.
float4 Undimmed(float4 u,float dim){return float4(u.rgb,saturate((u.a-dim)/(1-dim)));}
[numthreads(8,8,1)] void Over(uint3 id:SV_DispatchThreadID){
    if(id.x>=W || id.y>=H)return;
    float2 p=float2(id.xy)+0.5;float2 layer=float2(W,H);float2 centre=layer*0.5;
    float dim=Dim();
    float4 c=Frame.Load(int3(id.xy,0));c.rgb*=1-dim;
    float2 s=LayerPixel(p,HudRect,layer);
    if(InLayer(s) && !(length(s-centre)<CrossRadius || InPiece(s)))c.rgb=OverPremultiplied(c.rgb,Undimmed(Ui.Load(int3(s,0)),dim));
    // The crosshair: the layer's centre drawn at the aim point, at the layer's scale.
    s=centre+(p-(HudRect.xy+HudRect.zw*0.5)-float2(AimX,AimY))*layer/HudRect.zw;
    if(length(s-centre)<CrossRadius && !InPiece(s))c.rgb=OverPremultiplied(c.rgb,Undimmed(Ui.Load(int3(s,0)),dim));
    Out[id.xy]=c;}
