import 'dart:io';
import 'package:logging/logging.dart';
import 'dart:typed_data';
import 'dart:convert';
import 'dart:async';

final Logger log = Logger('Socks5Proxy');

// Enum for address types
enum Addr { v4, v6, domain }

// Handler for SOCKS5 server connections
class Socks5ServerHandler {
  final Socket client;
  Socket? remoteSocket;

  States currentState = States.handshake;

  Socks5ServerHandler(this.client);

  void start() {
    client.listen((data) {
      processData(data);
    }, onDone: () {
      log.info('Client disconnected');
      client.close();
      remoteSocket?.destroy();
    }, onError: (error) {
      log.severe('Client error: $error');
      client.close();
      remoteSocket?.destroy();
    });
  }

  void startProxy() {
    //TODO: when remoteSocket is null it wont listen here. that might be not so good. please double check
    remoteSocket?.listen((data) {
      client.add(data);
    }, onDone: () {
      client.destroy();
    });
  }

  void processData(List<int> data) async {
    switch (currentState) {
      case States.handshake:
        print(data.length);
        if (data.length < 2) return;
        final version = data[0];
        if (version != 5) return;
        // Respond with method selection
        Uint8List temp = Uint8List.fromList([5, 0]);
        client.add(temp); // SOCKS5 version and method (no authentication)
        currentState = States.handling;
        break;

      case States.handling:
        print(data.length);
        if (data.length < 4) return;
        final version = data[0];
        if (version != 5) {
          client.close();
          return;
        }

        final cmd = data[1];
        if (cmd != 1) {
          log.warning('Unsupported command: $cmd');
          client.close();
          return;
        }
        final reserved = data[2]; // Reserved byte
        final addrType = data[3];

        String address;
        int port = 0;

        switch (addrType) {
          case 0x01: // IPv4
            address = data.sublist(4, 8).join('.');
            port = (data[8] << 8) | data[9];
            break;
          case 0x04: // IPv6
            address = '::1';
            port = (data[16] << 8) | data[17];
            break;
          case 0x03: // Domain name
            final domainLength = data[4];
            address = String.fromCharCodes(data.sublist(5, 5 + domainLength));
            port = (data[5 + domainLength] << 8) | data[6 + domainLength];
            break;
          default:
            log.warning('Unknown address type: $addrType');
            client.close();
            return;
        }

        try {
          remoteSocket = await Socket.connect(address, port,
              timeout: Duration(seconds: 5));
          print('Connected to $address:$port');

          // Reply: succeeded
          client.add(Uint8List.fromList([5, 0, 0, 1, 0, 0, 0, 0, 0, 0]));
          await client.flush();
        } catch (e) {
          print('Connection to $address:$port failed: $e');
          client.close();
        }
        currentState = States.proxying;
        startProxy();
        break;

      case States.proxying:
        try {
          remoteSocket?.add(data);
        } catch (e) {
          print('[-] failed: $e');
          client.close();
        }

        break;
    }
  }

  String parseAddress(List<int> data, int addrType) {
    switch (addrType) {
      case 0x01: // IPv4
        return data.sublist(4, 8).join('.');
      case 0x04: // IPv6
        return '::1';
      case 0x03: // Domain name
        final domainLength = data[4];
        return String.fromCharCodes(data.sublist(5, 5 + domainLength));
      default:
        throw Exception('Unknown address type: $addrType');
    }
  }
}

// Handler for TCP transfer and relaying data
class TcpTransferHandler {
  final String address;
  final Addr addr;
  final int port;
  late final Socket remSocket;

  TcpTransferHandler(this.address, this.addr, this.port);

  Future<void> connectAndRelay({required Socket clientSocket}) async {
    try {
      Socket remSocket = await _connectToRemote();
      _sendSocks5Reply(clientSocket, remSocket);

      _relayTraffic(clientSocket, remSocket);
    } catch (e) {
      log.severe('Connection failed: $e');
      clientSocket.close();
    }
  }

  Future<Socket> _connectToRemote() async {
    late Socket remSocket;
    switch (addr) {
      case Addr.v4:
        remSocket = await Socket.connect(address, port);
        break;
      case Addr.v6:
        remSocket = await Socket.connect(address, port);
        break;
      case Addr.domain:
        remSocket = await Socket.connect(address, port);
        break;
      default:
        throw Exception('Unsupported address type');
    }
    return remSocket;
  }

  void _sendSocks5Reply(Socket clientSocket, Socket remSocket) {
    final reply = BytesBuilder()
      ..addByte(5) // SOCKS version
      ..addByte(0) // Connection succeeded
      ..addByte(0) // Reserved

      ..addByte(remSocket.address.type == InternetAddressType.IPv4
          ? 1
          : 4) // Address type

      ..add(remSocket.address.rawAddress) // Address
      ..addByte(remSocket.port >> 8) // Port (high byte)
      ..addByte(remSocket.port & 0xFF); // Port (low byte)

    clientSocket.add(reply.toBytes());
  }

  void _relayTraffic(Socket clientSocket, Socket remSocket) {
    clientSocket.listen(
          (data) {
        remSocket.add(data);
      },
      onError: (error) {
        log.severe('Client socket error: $error');
        remSocket.close();
      },
      onDone: () {
        remSocket.close();
      },
    );
    print('[+]TcpTransferHandler _relayTraffic');
    // Listen for data from the remote socket and forward it to the client socket
    remSocket.listen(
          (data) {
        clientSocket.add(data);
      },
      onError: (error) {
        log.severe('Remote socket error: $error');
        clientSocket.close();
      },
      onDone: () {
        clientSocket.close();
      },
    );
  }
}

enum States { handshake, handling, proxying }
