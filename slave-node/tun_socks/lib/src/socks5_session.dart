import 'dart:io';
import 'package:synchronized/synchronized.dart';
import 'dart:typed_data';
import 'dart:convert';
import 'dart:async';

// Enum for address types
enum Addr { v4, v6, domain }
enum States { handshake, handling, proxying }

// SOCKS5 session list
Map<int, Socks5Session> sessions = {};

// Handler for SOCKS5 server connections
class Socks5Session {
  final int sessionId;
  final Socket? _masterConn;
  Socket? _destConn;
  final Lock _destConnLock = Lock();
  States currentState = States.handshake;
  final Function(int, List<int>) _sendDataToMaster;

  Socks5Session(this.sessionId, this._masterConn, this._sendDataToMaster);

  Future<void> processSocks5Data(List<int> data) async {
    try {
        switch (currentState) {
          case States.handshake:
            if (data.length < 2) {
              _onSessionError('Invalid handshake data length');
              return;
            }

            if (data[0] != 5) {
              _onSessionError('Unsupported SOCKS version');
              return;
            }

            await _sendToClient(Uint8List.fromList([5, 0])); // No authentication required
            currentState = States.handling;
            break;

          case States.handling:
            if (data.length < 4) {
              _onSessionError('Invalid handling data length');
              return;
            }

            if (data[0] != 5) {
              _onSessionError('Invalid handling SOCKS version');
              return;
            }

            if (data[1] != 1) {
              _onSessionError('Unsupported command (not CONNECT)');
              return;
            }

            // Secondary byte is reserved one
            final addrType = data[3];

            String address;
            int port;

            switch (addrType) {
              case 0x01: // IPv4
                address = data.sublist(4, 8).join('.');
                port = (data[8] << 8) | data[9];
                break;

              case 0x04: // IPv6
                address = data.sublist(4, 20).map((b) => b.toRadixString(16)).join(':');
                port = (data[20] << 8) | data[21];
                break;

              case 0x03: // Domain name
                final domainLength = data[4];
                address = String.fromCharCodes(data.sublist(5, 5 + domainLength));
                port = (data[5 + domainLength] << 8) | data[6 + domainLength];
                break;

              default:
                _onSessionError('Unsupported address type: $addrType');
                return;
            }

            try {
              _destConn = await Socket.connect(address, port, timeout: Duration(seconds: 10));
              currentState = States.proxying;

              _destConn?.listen(
                (data) => _sendToClient(data),
                onDone: _onSessionClosed,
                onError: (error) => _onSessionError('Destination connection error: $error'),
              );

              await _sendToClient(Uint8List.fromList([5, 0, 0, 1, 0, 0, 0, 0, 0, 0]));
            } catch (e) {
              _onSessionError('Failed to connect to $address:$port: $e');
            }
            break;

          case States.proxying:
            await _sendToDest(data);
            break;
      }
    } catch (e) {
      _onSessionError('Unexpected error in session $sessionId: $e');
    }
  }

  Future<void> _sendToClient(List<int> data) async {
    try {
      await _sendDataToMaster(sessionId, data);
    } catch (e) {
      _onSessionError('Failed to send data to master: $e');
    }
  }

  Future<void> _sendToDest(List<int> data) async {
    await _destConnLock.synchronized(() async {
      if (_destConn != null) {
        try {
          _destConn?.add(data);
          await _destConn?.flush();
        } catch (e) {
          _onSessionError('Failed to send data to destination: $e');
        }
      }
    });
  }

  void _onSessionError(String message) {
    print(message);
    _closeSession();
  }

  void _onSessionClosed() {
    _closeSession();
  }

  void _closeSession() async {
    await _destConnLock.synchronized(() async {
      await _destConn?.close();
      _destConn = null;
    });

    // Remove session from sessions map
    sessions.remove(sessionId);
  }
}

