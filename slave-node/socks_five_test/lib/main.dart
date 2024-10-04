import 'package:flutter/material.dart';
import 'package:http/http.dart' as http;
import 'dart:io';
import 'dart:typed_data';
import 'socks5_server.dart';

void main() {
  runApp(MyApp());
}

// Enum for address types
enum Addr { v4, v6, domain }
enum States { handshake, handling, proxying }

class MyApp extends StatelessWidget {
  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: MyHomePage(),
    );
  }
}

class MyHomePage extends StatefulWidget {
  @override
  _MyHomePageState createState() => _MyHomePageState();
}

class _MyHomePageState extends State<MyHomePage> {
  Socket? _master_conn;
  Socket? _dest_conn;
  bool _isConnected = false;
  bool _isConnecting = false;
  bool _isManualyStopped = false;
  int _retryAttempts = 0;
  String _connectionStatus = "";

  States currentState = States.handshake;

  // Perform Internet speed test
  bool _inCheckingSpeedStage = false;
  final int _expectedDataSize = 5000000;
  int _recvBytes = 0;
  Stopwatch? _checkTimer;

  @override
  void initState() {
    super.initState();
  }

  // Retry logic to reconnect when connection is broken
  Future<void> _retryConnection() {
    // Retry logic to reconnect when connection is broken
    if (_retryAttempts % 5 != 0 || _retryAttempts == 0) {
      // Retry the connection every 5 seconds
      Future.delayed(Duration(seconds: 5), () async {
        _retryAttempts++;
        print('Retrying connection... Attempt $_retryAttempts');
        await _startProxyServer();
      });
    } else {
      // After every 5 attempts, wait for 1 minute
      print('Reached 5 attempts. Waiting for 1 minute before retrying.');
      Future.delayed(Duration(minutes: 1), () async {
        _retryAttempts++;
        print('Resuming retries after 1 minute. Attempt $_retryAttempts');
        await _startProxyServer();
      });
    }

    return Future.value();
  }

  Future<double> _performSpeedTest(String url) async {
    final stopwatch = Stopwatch()..start();

    try {
      final response = await http.get(Uri.parse(url));
      stopwatch.stop();

      if (response.statusCode == 200) {
        final contentLength = response.contentLength ?? 0;
        final elapsedTime = stopwatch.elapsedMilliseconds / 1000; // in seconds

        // Calculate speed in Mbps
        final speedMbps = (contentLength * 8) / (elapsedTime * 1000000); 
        return speedMbps;
      } else {
        print('Failed to download file: ${response.statusCode}');
        return 0.0;
      }
    } catch (e) {
      print('Error during speed test: $e');
      return 0.0;
    }
  } 

  Future<void> _startProxyServer() async {
    setState(() {
      _isConnecting = true;
      _isManualyStopped = false;
      _connectionStatus = "Connecting...";
    });

    final host = '188.245.104.81';
    // final host = '192.168.12.244';

    final port = 8000;
    
    try {
      _master_conn = await Socket.connect(host, port);
      print('Connected to master node: $host:$port');

      setState(() {
        _isConnected = true;
        _isConnecting = false;
        _connectionStatus = "Connected to master node, waiting for client requests...";
      });

      _master_conn!.listen(
        (data) {
          _processData(data);
        },
        onDone: () {
          print('Master connection closed');
          setState(() {
            _isConnected = false;
            _isConnecting = false;
            _connectionStatus = "Connection closed";
          });

          if (!_isManualyStopped)
            _retryConnection();
        },
        onError: (error) {
          print('Master connection error: $error');
          _master_conn?.close();
          _dest_conn?.destroy();
          setState(() {
            _isConnected = false;
            _isConnecting = false;
            _connectionStatus = "Connection error, trying to reconnect...";
          });
          _retryConnection();
        },
      );
    } catch (e) {
      print('Failed to connect to primary connection: $e');
      setState(() {
        _isConnecting = false;
        _connectionStatus = "Failed to connect, trying to reconnect...";
      });
      _retryConnection();
    }
    return Future.value();
  }

  void _processData(List<int> data) async {
    try {
        String command = String.fromCharCodes(data).trim();

        if (command.startsWith('SPEED_TEST')) {
          // Master sends SPEED_TEST command with the URL
          String url = command.split(' ')[1];
          print('Received speed test command. Testing URL: $url');

          // Perform speed test
          double speedMbps = await _performSpeedTest(url);
          print('Speed test result: $speedMbps Mbps');

          // Send result back to the master
          Uint8List result = Uint8List.fromList('SPEED $speedMbps\n'.codeUnits);
          _master_conn?.add(result);
          await _master_conn?.flush();
          return;
        }

        switch (currentState) {
          case States.handshake:
            if (data.length < 2) return;
            final version = data[0];
            if (version != 5) return;
            // Respond with method selection
            Uint8List temp = Uint8List.fromList([5, 0]);
            _master_conn?.add(temp); // SOCKS5 version and method (no authentication)
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
              print('Client request RESOLVED: $address:$port');

              // Reply: succeeded
              _master_conn?.add(Uint8List.fromList([5, 0, 0, 1, 0, 0, 0, 0, 0, 0]));
              await _master_conn?.flush();
            } catch (e) {
              print('Connection to $address:$port failed to resolve: $e');
              _master_conn?.close();
            }
            currentState = States.proxying;

            setState(() {
              _connectionStatus = "Handling proxy stream data";
            });

            _dest_conn?.listen((data) {
              _master_conn?.add(data);
            }, onDone: () {
              _dest_conn?.destroy();
              setState(() {
                _connectionStatus = "Waiting for client requests...";
              });
            });

            break;

          case States.proxying:
            _dest_conn?.add(data);
            break;
      }
    } catch (e) {
      print('Error processing data: $e');
      _master_conn?.close();
    }
  }

  // Send speed acknowledgment to the server
  void _sendSpeedAcknowledgment(double elapsedTime) {
    if (_master_conn != null) {
      String timeMessage = elapsedTime.toStringAsFixed(2);
      _master_conn!.write(timeMessage);  // Send the time taken to the server
      _inCheckingSpeedStage = false;
      print('Sent time acknowledgment to server: $timeMessage');
    }
  }

  // Stop proxy server and close connection
  void _stopProxyServer() {
    if (_master_conn != null) {
      _master_conn!.close();
      setState(() {
        _isConnected = false;
        _isManualyStopped = true;
        _connectionStatus = "Disconnected";
      });
      print('Proxy server stopped');
    }
  }

  @override
  void dispose() {
    _master_conn?.destroy();
    _dest_conn?.destroy();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text('Socket Proxy Example'),
      ),
      body: Center(
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            // Connection status bar
            Padding(
              padding: const EdgeInsets.all(16.0),
              child: Text(
                '$_connectionStatus',
                style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
              ),
            ),
            // Stop Proxy Button
            ElevatedButton(
              onPressed: _isConnected ? _stopProxyServer : null,
              child: Text('Stop Proxy'),
            ),
            SizedBox(height: 20),
            // Start Proxy Button
            FloatingActionButton(
              onPressed: (!_isConnected && !_isConnecting) ? _startProxyServer : null,
              tooltip: 'Start Proxy',
              child: Icon(Icons.play_arrow),
            ),
          ],
        ),
      ),
    );
  }
}
