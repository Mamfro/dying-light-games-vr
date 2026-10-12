// The dynamic HUD's pieces taken out of an eye frame that has the HUD drawn in (hudfix.rs): within a
// piece's rectangle, the world under the HUD is recovered from the UI layer where the HUD lets any of
// it through (frame = world * (1 - alpha) + ui), and where the HUD is solid the nearest pixel on the
// row that is not solid stands in. In place: the rectangle is copied out of the frame first.
cbuffer Fix:register(b0){uint W,H,X,Y,RW,RH,Pad0,Pad1;};
Texture2D<float4> Ui:register(t0);Texture2D<float4> Frame:register(t1);
RWTexture2D<unorm float4> Out:register(u0);
float4 World(int2 p){float4 u=Ui.Load(int3(p,0));float4 c=Frame.Load(int3(p,0));if(u.a<0.98)c.rgb=Unblend(c.rgb,u);return c;}
[numthreads(8,8,1)] void Fix(uint3 id:SV_DispatchThreadID){
    if(id.x>=RW || id.y>=RH)return;
    int2 p=int2(X+id.x,Y+id.y);
    if(p.x>=int(W) || p.y>=int(H))return;
    float4 u=Ui.Load(int3(p,0));
    if(u.a<0.98){Out[p]=World(p);return;}
    [loop] for(int r=1;r<=192;++r){
        int xl=p.x-r,xr=p.x+r;
        if(xl>=0 && Ui.Load(int3(xl,p.y,0)).a<0.98){Out[p]=World(int2(xl,p.y));return;}
        if(xr<int(W) && Ui.Load(int3(xr,p.y,0)).a<0.98){Out[p]=World(int2(xr,p.y));return;}}
    Out[p]=Frame.Load(int3(p,0));}
