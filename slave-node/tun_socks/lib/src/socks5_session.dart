import 'dart:io';
import 'package:synchronized/synchronized.dart';
import 'dart:typed_data';
import 'dart:convert';
import 'dart:async';
import 'dart:collection';

// SOCKS5 session list
Map<int, Socks5Session> sessions = {};

// Handler for SOCKS5 server connections
class Socks5Session {
  final int sessionId;
  Socket? _destConn;
  final Lock _destConnLock = Lock();
  final Function(int, List<int>) _sendDataToMaster;
  // Completer to signal readiness
  final Completer<void> _connectionReady = Completer<void>();
  // Packet queue for initial processing
  final Queue<List<int>> _packetQueue = Queue();

  Socks5Session(this.sessionId, this._sendDataToMaster);

  /// Initialize the session and connect to the destination
  Future<void> initialize(String destAddress, int destPort) async {
    try {
      _destConn = await Socket.connect(destAddress, destPort, timeout: Duration(seconds: 10));

      _destConn?.listen(
        (data) => _sendToClient(data),
        onDone: _onSessionClosed,
        onError: (error) => _onSessionError('Destination connection error: $error'),
      );

      _connectionReady.complete();
      _processQueuedPackets();
    } catch (e) {
      _onSessionError('Failed to connect to $destAddress:$destPort: $e');
    }
  }

  Future<void> processSocks5Data(List<int> data) async {
    if (!_connectionReady.isCompleted) {
      // Queue data until initialization is ready
      print('Session $sessionId not initialized yet. Buffering packet.');
      _packetQueue.add(data);
      return;
    }

    try {
      await _sendToDest(data);
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
    await _connectionReady.future;
    await _destConnLock.synchronized(() async {
      if (_destConn != null) {
        try {
          _destConn?.add(data);
          await _destConn?.flush();
        } on SocketException catch (e) {
          print('Failed to send data to destination: $e');
          closeSession();
        } catch (e) {
          _onSessionError('Failed to send data to destination: $e');
        }
      } else {
        print("destConn for session $sessionId is not available");
      }
    });
  }

  void _processQueuedPackets() {
    while (_packetQueue.isNotEmpty) {
      final data = _packetQueue.removeFirst();
      processSocks5Data(data);
    }
  }

  void _onSessionError(String message) {
    print(message);
    closeSession();
  }

  void _onSessionClosed() {
    closeSession();
  }

  void closeSession() async {
    // Remove session from sessions map
    sessions.remove(sessionId);

    if (_destConn == null) {
      return;
    }

    await _destConnLock.synchronized(() async {
      await _destConn?.close();
      _destConn = null;
    });
  }
}

