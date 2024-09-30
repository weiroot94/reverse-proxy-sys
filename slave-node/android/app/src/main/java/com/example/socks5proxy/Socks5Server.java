/*
 * Click nbfs://nbhost/SystemFileSystem/Templates/Licenses/license-default.txt to change this license
 * Click nbfs://nbhost/SystemFileSystem/Templates/Classes/Class.java to edit this template
 */
package com.example.socks5proxy;

import io.netty.bootstrap.Bootstrap;
import io.netty.bootstrap.ServerBootstrap;
import io.netty.channel.*;
import io.netty.channel.nio.NioEventLoopGroup;
import io.netty.channel.socket.SocketChannel;
import io.netty.channel.socket.nio.NioSocketChannel;
import io.netty.channel.socket.nio.NioServerSocketChannel;
import io.netty.handler.codec.LengthFieldBasedFrameDecoder;
import io.netty.handler.codec.LengthFieldPrepender;
import io.netty.handler.logging.LogLevel;
import io.netty.handler.logging.LoggingHandler;
import io.netty.buffer.ByteBuf;
import io.netty.buffer.Unpooled;
import io.netty.util.CharsetUtil;

import java.nio.charset.StandardCharsets;
import java.util.logging.Logger;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.Inet6Address;

import java.net.SocketAddress;
import java.net.UnknownHostException;
import java.util.logging.Level;

/**
 * @author KDark
 */
enum Addr {
    V4,
    V6,
    DOMAIN
}

public class Socks5Server {

    private static final Logger log = Logger.getLogger(Socks5Server.class.getName());
//    private static final byte[] MAGIC_FLAG = {0x37, 0x37};

    public static void main(String[] args) throws InterruptedException {
        // Set up the server
        EventLoopGroup bossGroup = new NioEventLoopGroup();
        EventLoopGroup workerGroup = new NioEventLoopGroup();
        try {
            ServerBootstrap bootstrap = new ServerBootstrap();
            bootstrap.group(bossGroup, workerGroup)
                    .channel(NioServerSocketChannel.class)
                    .childHandler(new Socks5ServerInitializer());

            ChannelFuture future = bootstrap.bind(1080).sync();
            future.channel().closeFuture().sync();
        } finally {
            bossGroup.shutdownGracefully();
            workerGroup.shutdownGracefully();
        }
    }

    static class Socks5ServerInitializer extends ChannelInitializer<SocketChannel> {

        @Override
        protected void initChannel(SocketChannel ch) {
            ChannelPipeline pipeline = ch.pipeline();
            pipeline.addLast(new LoggingHandler(LogLevel.INFO));
            pipeline.addLast(new LengthFieldBasedFrameDecoder(1024, 0, 2, 0, 2));
            pipeline.addLast(new LengthFieldPrepender(2));
            pipeline.addLast(new Socks5ServerHandler());
        }
    }

    static class Socks5ServerHandler extends SimpleChannelInboundHandler<ByteBuf> {

        private enum State {
            HANDSHAKE,
            HANDLING,
            RECEIVE_4_BYTES
        }

        private static final int SOCKS_VERSION = 5;
        private State currentState = State.HANDSHAKE;

        @Override
        public void channelReadComplete(ChannelHandlerContext ctx) {
            ctx.flush();
        }

        @Override
        protected void channelRead0(ChannelHandlerContext ctx, ByteBuf msg) throws Exception {
            switch (currentState) {
                case HANDSHAKE: {
//                    int length = msg.readableBytes();
                    // Read the initial SOCKS5 handshake
                    if (msg.readableBytes() < 2) {
                        return;
                    }

                    byte version = msg.readByte();
//                    byte nmethods = msg.readByte();

                    if (version != SOCKS_VERSION) {
                        return;
                    }
                    // Respond with method selection
                    ctx.writeAndFlush(Unpooled.copiedBuffer(new byte[]{SOCKS_VERSION, 0}));
                    currentState = State.HANDLING;
                }
                case HANDLING: {
                    if (msg.readableBytes() < 4) {
                        return;
                    }
                    byte version = msg.readByte();
                    if (version != SOCKS_VERSION) {
                        log.warning("Unsupported command: " + version);
                        ctx.close();
                        return;
                    }

                    byte cmd = msg.readByte();
                    if (cmd != 1) {
                        log.warning("Unsupported command: " + cmd);
                        ctx.close();
                        return;
                    }

                    byte reserved = msg.readByte();
                    byte addrType = msg.readByte();

                    InetSocketAddress remoteAddress;
                    int port = 0;
                    switch (addrType) {
                        case 0x01: // IPv4
                            byte[] ipv4 = new byte[4];
                            msg.readBytes(ipv4);
                            port = msg.readUnsignedShort(); // Correctly read port as unsigned short
                            remoteAddress = new InetSocketAddress(InetAddress.getByAddress(ipv4), port);
                            break;
                        case 0x04: // IPv6
                            byte[] ipv6 = new byte[16];
                            msg.readBytes(ipv6);
                            port = msg.readUnsignedShort(); // Correctly read port as unsigned short
                            remoteAddress = new InetSocketAddress(Inet6Address.getByAddress(ipv6), port);
                            break;
                        case 0x03: // Domain name
                            byte domainLength = msg.readByte();
                            byte[] domain = new byte[domainLength];
                            msg.readBytes(domain);
                            port = msg.readUnsignedShort(); // Correctly read port as unsigned short
                            remoteAddress = new InetSocketAddress(new String(domain, CharsetUtil.UTF_8), port);
                            break;
                        default:
                            log.warning("Unknown address type: " + addrType);
                            ctx.close();
                            return;
                    }
                    ChannelPipeline pipeline = ctx.pipeline();
                    TcpTransferHandler transferHandler = new TcpTransferHandler(remoteAddress.getHostString(), Addr.V4, port);
                    pipeline.addLast(transferHandler);

                    pipeline.remove(this);
                    transferHandler.channelActive(ctx);

                    currentState = State.HANDLING;
                }
            }
        }
    }
}


class TcpTransferHandler extends ChannelInboundHandlerAdapter {

    private final String address;
    private final Addr addr;
    private final int port;

    public TcpTransferHandler(String address, Addr addr, int port) {
        this.address = address;
        this.addr = addr;
        this.port = port;
    }

    @Override
    public void channelActive(ChannelHandlerContext ctx) {
        ctx.channel().eventLoop().execute(() -> {
            try {
                connectToRemote(ctx);
            } catch (UnknownHostException ex) {
                Logger.getLogger(TcpTransferHandler.class.getName()).log(Level.SEVERE, null, ex);
            }
        });
    }

    private void connectToRemote(ChannelHandlerContext ctx) throws UnknownHostException, UnknownHostException {
        System.out.println("Proxy connect to " + address);

        Bootstrap bootstrap = new Bootstrap();
        bootstrap.group(new NioEventLoopGroup())
                .channel(NioSocketChannel.class)
                .handler(new LoggingHandler(LogLevel.INFO))
                .handler(new ChannelInitializer<SocketChannel>() {
                    @Override
                    protected void initChannel(SocketChannel ch) {
                        // Optionally add more handlers
                    }
                });

        ChannelFuture connectFuture;
        switch (addr) {
            case V4:
                connectFuture = bootstrap.connect(new InetSocketAddress(address, port));
                break;
            case V6: {
                Inet6Address ipv6Address = null;
                try {
                    ipv6Address = (Inet6Address) Inet6Address.getByName(address);
                } catch (UnknownHostException ex) {
                    Logger.getLogger(TcpTransferHandler.class.getName()).log(Level.SEVERE, null, ex);
                }
                connectFuture = bootstrap.connect(new InetSocketAddress(ipv6Address, port));
                break;
            }

            case DOMAIN:
                connectFuture = bootstrap.connect(new InetSocketAddress(address, port));
                break;
            default: {
                System.out.println("Unsupported address type");
                return;
            }
        }

        connectFuture.addListener((ChannelFutureListener) future -> {
            if (!future.isSuccess()) {
                System.out.println("Connection to " + address + " failed");
                ctx.close();
                return;
            }

            System.out.println("Connected to " + address);
            SocketChannel remoteChannel = (SocketChannel) future.channel();
            remoteChannel.config().setKeepAlive(true);
//            remoteChannel.config().setOption(ChannelOption.SO_KEEPALIVE, true);

            sendSocks5Reply(ctx, remoteChannel.localAddress());

            // Start the data relay between client and remote server
            relayTraffic(ctx, remoteChannel);
        });
    }

    private void sendSocks5Reply(ChannelHandlerContext ctx, SocketAddress localAddress) {
        ByteBuf reply = Unpooled.buffer();
        reply.writeByte(5);  // SOCKS version
        reply.writeByte(0);  // Connection succeeded
        reply.writeByte(0);  // Reserved

        if (localAddress instanceof InetSocketAddress) {
            InetSocketAddress inetAddress = (InetSocketAddress) localAddress;
            if (inetAddress.getAddress() instanceof java.net.Inet4Address) {
                reply.writeByte(1);  // IPv4
                reply.writeBytes(inetAddress.getAddress().getAddress());
            } else if (inetAddress.getAddress() instanceof Inet6Address) {
                reply.writeByte(4);  // IPv6
                reply.writeBytes(inetAddress.getAddress().getAddress());
            } else {
                String host = inetAddress.getHostName();
                reply.writeByte(3);  // Domain name
                reply.writeByte(host.length());
                reply.writeBytes(host.getBytes(StandardCharsets.UTF_8));
            }
            reply.writeShort(inetAddress.getPort());
        }

        ctx.writeAndFlush(reply);
    }

    private void relayTraffic(ChannelHandlerContext ctx, SocketChannel remoteChannel) {
        ctx.pipeline().addLast(new RelayHandler(remoteChannel));
        remoteChannel.pipeline().addLast(new RelayHandler(ctx.channel()));
    }

    private static class RelayHandler extends ChannelInboundHandlerAdapter {
        private final Channel relayChannel;

        RelayHandler(Channel relayChannel) {
            this.relayChannel = relayChannel;
        }

        @Override
        public void channelRead(ChannelHandlerContext ctx, Object msg) {
            relayChannel.writeAndFlush(msg);
        }

        @Override
        public void channelInactive(ChannelHandlerContext ctx) {
            if (relayChannel.isActive()) {
                relayChannel.close();
            }
        }

        @Override
        public void exceptionCaught(ChannelHandlerContext ctx, Throwable cause) {
            cause.printStackTrace();
            ctx.close();
        }
    }
}