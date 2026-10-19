.class public LAllOpcodes;
.super Ljava/lang/Object;

# Every Dalvik opcode in dex 039, with operands chosen to catch decoder bugs:
# distinct registers in every field (so a swapped field shows), the largest
# register each field can hold, negative and extreme literals, backward and
# forward branches, and switch and array payloads. It is assembled with
# `smali assemble --api 28` and decoded by dex-reader, dexlib2 and baksmali;
# it is never run, so it need not verify.

.field public i:I
.field public j:J
.field public o:Ljava/lang/Object;
.field public z:Z
.field public b:B
.field public c:C
.field public s:S
.field public static si:I
.field public static sj:J
.field public static so:Ljava/lang/Object;
.field public static sz:Z
.field public static sb:B
.field public static sc:C
.field public static ss:S

.method public static moves()V
    .registers 65535
    nop
    move v15, v14
    move/from16 v255, v65534
    move/16 v65533, v65532
    move-wide v14, v12
    move-wide/from16 v254, v65530
    move-wide/16 v65520, v65510
    move-object v13, v11
    move-object/from16 v253, v40000
    move-object/16 v40001, v65534
    invoke-static {}, LAllOpcodes;->moves()V
    move-result v254
    invoke-static {}, LAllOpcodes;->moves()V
    move-result-wide v252
    invoke-static {}, LAllOpcodes;->moves()V
    move-result-object v255
    return-void
.end method

.method public static consts()V
    .registers 256
    const/4 v15, -8
    const/4 v14, 7
    const/4 v13, -1
    const/16 v255, -32768
    const/16 v254, 32767
    const v253, -2147483648
    const v252, 0x12345678
    const/high16 v251, 0x80000000
    const/high16 v250, 0x7fff0000
    const-wide/16 v248, -32768
    const-wide/32 v246, -2147483648
    const-wide v244, 0x8000000000000000L
    const-wide v242, 0x0123456789abcdefL
    const-wide v240, -2L
    const-wide/high16 v238, 0x8000000000000000L
    const-wide/high16 v236, -0x1000000000000L
    const-wide/high16 v234, 0x7fff000000000000L
    const-wide/high16 v230, 0x0L
    const-wide v228, 0x7fffffffL
    const-wide v226, 0x80000000L
    const-string v233, "a"
    const-string/jumbo v232, "b"
    const-class v231, LAllOpcodes;
    return-void
.end method

.method public static objects()V
    .registers 256
    monitor-enter v255
    monitor-exit v254
    check-cast v253, Ljava/lang/String;
    instance-of v15, v14, Ljava/lang/String;
    array-length v13, v12
    new-instance v252, LAllOpcodes;
    new-array v11, v10, [I
    filled-new-array {}, [I
    filled-new-array {v1}, [I
    filled-new-array {v1, v2, v3, v4, v5}, [I
    filled-new-array {v15, v14, v13, v12, v11}, [I
    filled-new-array/range {v200 .. v204}, [I
    filled-new-array/range {v255 .. v255}, [I
    throw v251
    return-void
.end method

.method public static arrays_payloads()V
    .registers 256
    fill-array-data v255, :bytes
    fill-array-data v254, :shorts
    fill-array-data v253, :ints
    fill-array-data v252, :longs
    packed-switch v251, :packed
    sparse-switch v250, :sparse
    packed-switch v249, :packed_neg
    return-void
:bytes
    .array-data 1
        0x1t 0x2t 0x3t -0x80t 0x7ft
    .end array-data
:shorts
    .array-data 2
        -0x8000s 0x7fffs 0x1s
    .end array-data
:ints
    .array-data 4
        -0x80000000 0x7fffffff 0x0
    .end array-data
:longs
    .array-data 8
        -0x8000000000000000L 0x7fffffffffffffffL 0x0L -0x2L 0x80000000L
    .end array-data
:packed
    .packed-switch 0x1
        :t1
        :t2
        :t3
    .end packed-switch
:packed_neg
    .packed-switch -0x80000000
        :t3
    .end packed-switch
:sparse
    .sparse-switch
        -0x80000000 -> :t1
        -0x1 -> :t2
        0x7fffffff -> :t3
    .end sparse-switch
:t1
    nop
:t2
    nop
:t3
    nop
    return-void
.end method

.method public static branches()V
    .registers 256
:back
    nop
    if-eq v15, v14, :back
    if-eq v13, v12, :fwd
    if-ne v15, v14, :back
    if-ne v13, v12, :fwd
    if-lt v15, v14, :back
    if-lt v13, v12, :fwd
    if-ge v15, v14, :back
    if-ge v13, v12, :fwd
    if-gt v15, v14, :back
    if-gt v13, v12, :fwd
    if-le v15, v14, :back
    if-le v13, v12, :fwd
    if-eqz v255, :back
    if-eqz v254, :fwd
    if-nez v255, :back
    if-nez v254, :fwd
    if-ltz v255, :back
    if-ltz v254, :fwd
    if-gez v255, :back
    if-gez v254, :fwd
    if-gtz v255, :back
    if-gtz v254, :fwd
    if-lez v255, :back
    if-lez v254, :fwd
    goto :back
    goto/16 :back
    goto/32 :back
    goto :fwd
    goto/16 :fwd
    goto/32 :fwd
    goto/32 :self
:self
    nop
:fwd
    nop
    cmpl-float v255, v254, v253
    cmpg-float v255, v254, v253
    cmpl-double v252, v248, v246
    cmpg-double v252, v248, v246
    cmp-long v252, v248, v246
    return-void
.end method

.method public static array_ops()V
    .registers 256
    aget v250, v255, v253
    aget-wide v248, v255, v253
    aget-object v246, v255, v253
    aget-boolean v244, v255, v253
    aget-byte v242, v255, v253
    aget-char v240, v255, v253
    aget-short v238, v255, v253
    aput v236, v255, v253
    aput-wide v234, v255, v253
    aput-object v232, v255, v253
    aput-boolean v230, v255, v253
    aput-byte v228, v255, v253
    aput-char v226, v255, v253
    aput-short v224, v255, v253
    return-void
.end method

.method public static fields()V
    .registers 256
    iget v14, v15, LAllOpcodes;->i:I
    iget-wide v14, v15, LAllOpcodes;->j:J
    iget-object v14, v15, LAllOpcodes;->o:Ljava/lang/Object;
    iget-boolean v14, v15, LAllOpcodes;->z:Z
    iget-byte v14, v15, LAllOpcodes;->b:B
    iget-char v14, v15, LAllOpcodes;->c:C
    iget-short v14, v15, LAllOpcodes;->s:S
    iput v14, v15, LAllOpcodes;->i:I
    iput-wide v14, v15, LAllOpcodes;->j:J
    iput-object v14, v15, LAllOpcodes;->o:Ljava/lang/Object;
    iput-boolean v14, v15, LAllOpcodes;->z:Z
    iput-byte v14, v15, LAllOpcodes;->b:B
    iput-char v14, v15, LAllOpcodes;->c:C
    iput-short v14, v15, LAllOpcodes;->s:S
    sget v254, LAllOpcodes;->si:I
    sget-wide v254, LAllOpcodes;->sj:J
    sget-object v254, LAllOpcodes;->so:Ljava/lang/Object;
    sget-boolean v254, LAllOpcodes;->sz:Z
    sget-byte v254, LAllOpcodes;->sb:B
    sget-char v254, LAllOpcodes;->sc:C
    sget-short v254, LAllOpcodes;->ss:S
    sput v254, LAllOpcodes;->si:I
    sput-wide v254, LAllOpcodes;->sj:J
    sput-object v254, LAllOpcodes;->so:Ljava/lang/Object;
    sput-boolean v254, LAllOpcodes;->sz:Z
    sput-byte v254, LAllOpcodes;->sb:B
    sput-char v254, LAllOpcodes;->sc:C
    sput-short v254, LAllOpcodes;->ss:S
    return-void
.end method

.method public static invokes()V
    .registers 65535
    invoke-virtual {v1}, LAllOpcodes;->m0()V
    invoke-virtual {v1, v2}, LAllOpcodes;->m1(I)V
    invoke-virtual {v1, v2, v3}, LAllOpcodes;->m2(II)V
    invoke-virtual {v1, v2, v3, v4}, LAllOpcodes;->m3(III)V
    invoke-virtual {v1, v2, v3, v4, v5}, LAllOpcodes;->m4(IIII)V
    invoke-virtual {v15, v14, v13, v12, v11}, LAllOpcodes;->m4(IIII)V
    invoke-super {v1}, LAllOpcodes;->m0()V
    invoke-super {v1, v2}, LAllOpcodes;->m1(I)V
    invoke-super {v1, v2, v3}, LAllOpcodes;->m2(II)V
    invoke-super {v1, v2, v3, v4}, LAllOpcodes;->m3(III)V
    invoke-super {v1, v2, v3, v4, v5}, LAllOpcodes;->m4(IIII)V
    invoke-super {v15, v14, v13, v12, v11}, LAllOpcodes;->m4(IIII)V
    invoke-direct {v1}, LAllOpcodes;->m0()V
    invoke-direct {v1, v2}, LAllOpcodes;->m1(I)V
    invoke-direct {v1, v2, v3}, LAllOpcodes;->m2(II)V
    invoke-direct {v1, v2, v3, v4}, LAllOpcodes;->m3(III)V
    invoke-direct {v1, v2, v3, v4, v5}, LAllOpcodes;->m4(IIII)V
    invoke-direct {v15, v14, v13, v12, v11}, LAllOpcodes;->m4(IIII)V
    invoke-static {v1}, LAllOpcodes;->m0()V
    invoke-static {v1, v2}, LAllOpcodes;->m1(I)V
    invoke-static {v1, v2, v3}, LAllOpcodes;->m2(II)V
    invoke-static {v1, v2, v3, v4}, LAllOpcodes;->m3(III)V
    invoke-static {v1, v2, v3, v4, v5}, LAllOpcodes;->m4(IIII)V
    invoke-static {v15, v14, v13, v12, v11}, LAllOpcodes;->m4(IIII)V
    invoke-interface {v1}, LAllOpcodes;->m0()V
    invoke-interface {v1, v2}, LAllOpcodes;->m1(I)V
    invoke-interface {v1, v2, v3}, LAllOpcodes;->m2(II)V
    invoke-interface {v1, v2, v3, v4}, LAllOpcodes;->m3(III)V
    invoke-interface {v1, v2, v3, v4, v5}, LAllOpcodes;->m4(IIII)V
    invoke-interface {v15, v14, v13, v12, v11}, LAllOpcodes;->m4(IIII)V
    invoke-static {}, LAllOpcodes;->m()V
    invoke-virtual/range {v300 .. v305}, LAllOpcodes;->m5(IIIII)V
    invoke-virtual/range {v65530 .. v65534}, LAllOpcodes;->m4(IIII)V
    invoke-super/range {v300 .. v305}, LAllOpcodes;->m5(IIIII)V
    invoke-super/range {v65530 .. v65534}, LAllOpcodes;->m4(IIII)V
    invoke-direct/range {v300 .. v305}, LAllOpcodes;->m5(IIIII)V
    invoke-direct/range {v65530 .. v65534}, LAllOpcodes;->m4(IIII)V
    invoke-static/range {v300 .. v305}, LAllOpcodes;->m5(IIIII)V
    invoke-static/range {v65530 .. v65534}, LAllOpcodes;->m4(IIII)V
    invoke-interface/range {v300 .. v305}, LAllOpcodes;->m5(IIIII)V
    invoke-interface/range {v65530 .. v65534}, LAllOpcodes;->m4(IIII)V
    invoke-static/range {}, LAllOpcodes;->m()V
    return-void
.end method

.method public static unops()V
    .registers 16
    neg-int v14, v1
    not-int v12, v3
    neg-long v10, v5
    not-long v14, v7
    neg-float v12, v9
    neg-double v10, v1
    int-to-long v14, v3
    int-to-float v12, v5
    int-to-double v10, v7
    long-to-int v14, v9
    long-to-float v12, v1
    long-to-double v10, v3
    float-to-int v14, v5
    float-to-long v12, v7
    float-to-double v10, v9
    double-to-int v14, v1
    double-to-long v12, v3
    double-to-float v10, v5
    int-to-byte v14, v7
    int-to-char v12, v9
    int-to-short v10, v1
    return-void
.end method

.method public static binops()V
    .registers 256
    add-int v250, v252, v254
    sub-int v250, v252, v254
    mul-int v250, v252, v254
    div-int v250, v252, v254
    rem-int v250, v252, v254
    and-int v250, v252, v254
    or-int v250, v252, v254
    xor-int v250, v252, v254
    shl-int v250, v252, v254
    shr-int v250, v252, v254
    ushr-int v250, v252, v254
    add-long v250, v252, v254
    sub-long v250, v252, v254
    mul-long v250, v252, v254
    div-long v250, v252, v254
    rem-long v250, v252, v254
    and-long v250, v252, v254
    or-long v250, v252, v254
    xor-long v250, v252, v254
    shl-long v250, v252, v254
    shr-long v250, v252, v254
    ushr-long v250, v252, v254
    add-float v250, v252, v254
    sub-float v250, v252, v254
    mul-float v250, v252, v254
    div-float v250, v252, v254
    rem-float v250, v252, v254
    add-double v250, v252, v254
    sub-double v250, v252, v254
    mul-double v250, v252, v254
    div-double v250, v252, v254
    rem-double v250, v252, v254
    add-int/2addr v14, v12
    sub-int/2addr v14, v12
    mul-int/2addr v14, v12
    div-int/2addr v14, v12
    rem-int/2addr v14, v12
    and-int/2addr v14, v12
    or-int/2addr v14, v12
    xor-int/2addr v14, v12
    shl-int/2addr v14, v12
    shr-int/2addr v14, v12
    ushr-int/2addr v14, v12
    add-long/2addr v14, v12
    sub-long/2addr v14, v12
    mul-long/2addr v14, v12
    div-long/2addr v14, v12
    rem-long/2addr v14, v12
    and-long/2addr v14, v12
    or-long/2addr v14, v12
    xor-long/2addr v14, v12
    shl-long/2addr v14, v12
    shr-long/2addr v14, v12
    ushr-long/2addr v14, v12
    add-float/2addr v14, v12
    sub-float/2addr v14, v12
    mul-float/2addr v14, v12
    div-float/2addr v14, v12
    rem-float/2addr v14, v12
    add-double/2addr v14, v12
    sub-double/2addr v14, v12
    mul-double/2addr v14, v12
    div-double/2addr v14, v12
    rem-double/2addr v14, v12
    add-int/lit16 v15, v13, -32768
    rsub-int v15, v13, 32767
    mul-int/lit16 v15, v13, -1
    div-int/lit16 v15, v13, 3
    rem-int/lit16 v15, v13, 5
    and-int/lit16 v15, v13, 32512
    or-int/lit16 v15, v13, -2
    xor-int/lit16 v15, v13, 1
    add-int/lit8 v255, v253, -128
    rsub-int/lit8 v255, v253, 127
    mul-int/lit8 v255, v253, -1
    div-int/lit8 v255, v253, 3
    rem-int/lit8 v255, v253, 5
    and-int/lit8 v255, v253, 127
    or-int/lit8 v255, v253, -2
    xor-int/lit8 v255, v253, 1
    shl-int/lit8 v255, v253, 31
    shr-int/lit8 v255, v253, -1
    ushr-int/lit8 v255, v253, 0
    return-void
.end method

.method public static method_handles()V
    .registers 65535
    invoke-polymorphic {v1}, Ljava/lang/invoke/MethodHandle;->invoke([Ljava/lang/Object;)Ljava/lang/Object;, ()V
    invoke-polymorphic {v15, v14, v13, v12, v11}, Ljava/lang/invoke/MethodHandle;->invokeExact([Ljava/lang/Object;)Ljava/lang/Object;, (IIII)V
    invoke-polymorphic/range {v300 .. v303}, Ljava/lang/invoke/MethodHandle;->invoke([Ljava/lang/Object;)Ljava/lang/Object;, (III)V
    invoke-polymorphic/range {v65530 .. v65534}, Ljava/lang/invoke/MethodHandle;->invokeExact([Ljava/lang/Object;)Ljava/lang/Object;, (IIII)I
    invoke-custom {v1, v2}, call_site_0("run", (II)V, "extra")@LAllOpcodes;->bsm(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;Ljava/lang/String;)Ljava/lang/invoke/CallSite;
    invoke-custom {v15, v14}, call_site_0("run", (II)V, "extra")@LAllOpcodes;->bsm(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;Ljava/lang/String;)Ljava/lang/invoke/CallSite;
    invoke-custom/range {v65533 .. v65534}, call_site_0("run", (II)V, "extra")@LAllOpcodes;->bsm(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;Ljava/lang/String;)Ljava/lang/invoke/CallSite;
    const-method-handle v255, invoke-static@Ljava/lang/Integer;->toString(I)Ljava/lang/String;
    const-method-handle v254, instance-get@LAllOpcodes;->i:I
    const-method-type v253, (IJ)Ljava/lang/String;
    return-void
.end method

.method public static exceptions()V
    .registers 16
:try_start
    throw v15
:try_end
    .catch Ljava/lang/Exception; {:try_start .. :try_end} :handler
    .catchall {:try_start .. :try_end} :handler
:handler
    move-exception v14
    return-void
    return-void
.end method

.method public static ret()I
    .registers 256
    return v255
.end method

.method public static retWide()J
    .registers 256
    return-wide v254
.end method

.method public static retObject()Ljava/lang/Object;
    .registers 256
    return-object v255
.end method

.method public static bsm(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;Ljava/lang/String;)Ljava/lang/invoke/CallSite;
    .registers 5
    const/4 v0, 0
    return-object v0
.end method
