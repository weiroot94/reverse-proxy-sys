import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

class ProxyClient {
  final String host;
  final int port;
  Socket? _socket;

  ProxyClient({required this.host, required this.port});

  Future<void> connect() async {
    try {
      _socket = await Socket.connect(host, port);
      print('Connected to $host:$port');

      _socket!.listen((data) {
        _handleData(data);
      }, onDone: () {
        print('Disconnected from server');
      }, onError: (error) {
        print('Connection error: $error');
      });

      // Send initial data if needed
      // For example, sending byte 55 as in your Java handler
      _socket!.add(Uint8List.fromList([55]));
    } catch (e) {
      print('Failed to connect: $e');
    }
  }

  void _handleData(Uint8List data) async {
    if (data.isNotEmpty) {
      int receivedByte = data[0];
      if (receivedByte == 55) {
          String host = '192.168.8.165'; // Replace with the actual host
          int port = 8000; // Replace with the actual port

          try {
            // Open a new connection (secondary channel)
            Socket secondarySocket = await Socket.connect(host, port);
            print('Connected to $host:$port (Secondary Channel)');

            // Handle the communication on the secondary channel (add handlers as needed)
            secondarySocket.listen((Uint8List data) {
              print('Received from secondary channel: ${data.length} bytes');
              // Process received data here
            });

            // Close the socket when done
            await secondarySocket.close();
          } catch (e) {
            print('Connection failed (Secondary Channel): $e');
          }
        }
      }
    }

  void disconnect() {
    _socket?.close();
    print('Disconnected from $host:$port');
  }
}
