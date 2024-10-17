import 'dart:io';
import 'package:logging/logging.dart';
import 'dart:typed_data';
import 'dart:convert';
import 'dart:async';

final Logger log = Logger('Socks5Session');

// Enum for address types
enum Addr { v4, v6, domain }
enum States { handshake, handling, proxying }

// SOCKS5 session list
Map<int, Socks5Session> sessions = {};

// Handler for SOCKS5 server connections
class Socks5Session {
  final int sessionId;
  final Socket? _master_conn;
  Socket? _dest_conn;

  States currentState = States.handshake;

  final Function(int, List<int>) _sendDataToMaster;

  Socks5Session(this.sessionId, this._master_conn, this._sendDataToMaster);

  Future<void> processSocks5Data(List<int> data) async {
    try {
        switch (currentState) {
          case States.handshake:
            if (data.length < 2) return;

            final version = data[0];
            if (version != 5) return;

            // Respond with method selection
            Uint8List handshakeRes = Uint8List.fromList([5, 0]);
            _sendData(handshakeRes);

            currentState = States.handling;
            break;

          case States.handling:
            if (data.length < 4) return;

            final version = data[0];
            if (version != 5) {
              _master_conn?.close();
              return;
            }

            final cmd = data[1];
            if (cmd != 1) {
              print('Unsupported command: $cmd');
              _master_conn?.close();
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
                address = data.sublist(4, 20).map((b) => b.toRadixString(16)).join(':');
                port = (data[20] << 8) | data[21];
                break;

              case 0x03: // Domain name
                final domainLength = data[4];
                address = String.fromCharCodes(data.sublist(5, 5 + domainLength));
                port = (data[5 + domainLength] << 8) | data[6 + domainLength];
                break;

              default:
                print('Unknown address type: $addrType');
                _master_conn?.close();
                return;
            }

            try {
              _dest_conn = await Socket.connect(address, port, timeout: Duration(seconds: 5));

              // Reply: succeeded
              _sendData(Uint8List.fromList([5, 0, 0, 1, 0, 0, 0, 0, 0, 0]));
              await _master_conn?.flush();
            } catch (e) {
              print('Connection to $address:$port failed to resolve: $e');
              _master_conn?.close();
            }
            currentState = States.proxying;

            _dest_conn?.listen((data) {
              _sendData(data);
            }, onDone: () {
              _closeSession();
            });

            break;

          case States.proxying:
            _dest_conn?.add(data);
            break;
      }
    } catch (e) {
      print('Error in session $sessionId: $e');
      _closeSession();
    }
  }

  void _sendData(List<int> data) {
    _sendDataToMaster(sessionId, data);
  }

  void _closeSession() {
    print('sessionId ${sessionId} EXIT');
    _dest_conn?.close();
    // Remove session from sessions map
    sessions.remove(sessionId);
  }
}

